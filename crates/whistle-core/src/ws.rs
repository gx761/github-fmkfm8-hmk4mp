//! WebSocket 帧级抓取与双向中继。
//!
//! [`relay`] 把一侧字节**原样**转发到另一侧（转发与解析解耦，解析出错绝不影响转发），
//! 同时用 [`FrameParser`] 增量解析帧，提取文本/控制消息记录到抓包。

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use whistle_capture::{CaptureStore, Traffic, WsMessage};

/// 单条消息负载最多抓取的字节数（超出则只记录大小）。
const MAX_CAPTURE_PAYLOAD: usize = 64 * 1024;
/// 每条流量最多保留的 WS 消息条数。
const MAX_WS_MESSAGES: usize = 500;

/// 把 `src` 的字节原样转发到 `dst`，同时解析 WebSocket 帧并记录消息。
pub(crate) async fn relay<R, W>(
    mut src: R,
    mut dst: W,
    dir: &'static str,
    traffic: Arc<Mutex<Traffic>>,
    store: Arc<CaptureStore>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut parser = FrameParser::default();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match src.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        // 先原样转发，保证 WS 连接绝不因抓取逻辑而损坏。
        if dst.write_all(&buf[..n]).await.is_err() {
            break;
        }
        let _ = dst.flush().await;

        parser.feed(&buf[..n], |opcode, payload, total| {
            let text = if opcode == 0x1 {
                Some(String::from_utf8_lossy(payload).into_owned())
            } else {
                None
            };
            let msg = WsMessage {
                dir: dir.to_string(),
                opcode,
                text,
                size: total,
            };
            let mut t = traffic.lock().unwrap();
            t.push_ws(msg, MAX_WS_MESSAGES);
            store.upsert(t.clone());
        });
    }
    let _ = dst.shutdown().await;
}

/// 增量 WebSocket 帧解析器（best-effort，用于抓取展示）。
#[derive(Default)]
pub(crate) struct FrameParser {
    buf: Vec<u8>,
    /// 分片消息累积负载与其操作码。
    msg_op: u8,
    msg_payload: Vec<u8>,
    /// 超大帧的剩余待丢弃字节数。
    skip_remaining: u64,
}

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
    total: u64,
}

impl FrameParser {
    /// 投喂新字节，对每个完整数据/控制消息调用 `emit(opcode, payload, total_size)`。
    pub(crate) fn feed(&mut self, data: &[u8], mut emit: impl FnMut(u8, &[u8], u64)) {
        self.buf.extend_from_slice(data);
        loop {
            // 处理超大帧的跳过模式。
            if self.skip_remaining > 0 {
                let drop = (self.skip_remaining as usize).min(self.buf.len());
                self.buf.drain(..drop);
                self.skip_remaining -= drop as u64;
                if self.skip_remaining > 0 {
                    return;
                }
            }
            match self.parse_frame() {
                Some(frame) => self.handle(frame, &mut emit),
                None => return,
            }
        }
    }

    fn handle(&mut self, f: Frame, emit: &mut impl FnMut(u8, &[u8], u64)) {
        match f.opcode {
            0x0 => {
                // 续帧：累积，FIN 时整体 emit。
                self.msg_payload.extend_from_slice(&f.payload);
                if f.fin {
                    let total = self.msg_payload.len() as u64;
                    emit(self.msg_op, &self.msg_payload, total);
                    self.msg_payload.clear();
                }
            }
            0x1 | 0x2 => {
                if f.fin {
                    emit(f.opcode, &f.payload, f.total);
                } else {
                    self.msg_op = f.opcode;
                    self.msg_payload = f.payload;
                }
            }
            // 控制帧（关闭/ping/pong）：各自 emit。
            0x8..=0xA => emit(f.opcode, &f.payload, f.total),
            _ => {}
        }
    }

    /// 尝试解析缓冲区开头的一个完整帧；不足则返回 None（等待更多字节）。
    fn parse_frame(&mut self) -> Option<Frame> {
        let buf = &self.buf;
        if buf.len() < 2 {
            return None;
        }
        let b0 = buf[0];
        let fin = b0 & 0x80 != 0;
        let opcode = b0 & 0x0f;
        let b1 = buf[1];
        let masked = b1 & 0x80 != 0;
        let len7 = (b1 & 0x7f) as usize;

        let (len, mut idx) = match len7 {
            126 => {
                if buf.len() < 4 {
                    return None;
                }
                (u16::from_be_bytes([buf[2], buf[3]]) as usize, 4)
            }
            127 => {
                if buf.len() < 10 {
                    return None;
                }
                let mut a = [0u8; 8];
                a.copy_from_slice(&buf[2..10]);
                (u64::from_be_bytes(a) as usize, 10)
            }
            n => (n, 2),
        };

        let mask = if masked {
            if buf.len() < idx + 4 {
                return None;
            }
            let m = [buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]];
            idx += 4;
            Some(m)
        } else {
            None
        };

        // 超大帧：不缓冲全部负载，进入跳过模式，仅记录大小。
        if len > MAX_CAPTURE_PAYLOAD {
            let avail = buf.len() - idx;
            let take = avail.min(len);
            self.skip_remaining = (len - take) as u64;
            self.buf.drain(..idx + take);
            return Some(Frame {
                fin,
                opcode,
                payload: Vec::new(),
                total: len as u64,
            });
        }

        if buf.len() < idx + len {
            return None; // 负载未到齐
        }
        let mut payload = buf[idx..idx + len].to_vec();
        if let Some(m) = mask {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= m[i % 4];
            }
        }
        self.buf.drain(..idx + len);
        Some(Frame {
            fin,
            opcode,
            payload,
            total: len as u64,
        })
    }
}

/// 编码一个（可选掩码的）文本帧，便于测试。
#[cfg(test)]
fn encode_text_frame(payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = vec![0x81u8]; // FIN + text
    let len = payload.len();
    let mask_bit = if mask.is_some() { 0x80 } else { 0 };
    assert!(len < 126, "测试仅覆盖短帧");
    out.push(mask_bit | len as u8);
    if let Some(m) = mask {
        out.extend_from_slice(&m);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ m[i % 4]));
    } else {
        out.extend_from_slice(payload);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unmasked_text_frame() {
        let mut p = FrameParser::default();
        let frame = encode_text_frame(b"hello", None);
        let mut msgs = Vec::new();
        p.feed(&frame, |op, payload, total| {
            msgs.push((op, String::from_utf8_lossy(payload).into_owned(), total));
        });
        assert_eq!(msgs, vec![(0x1, "hello".to_string(), 5)]);
    }

    #[test]
    fn parses_masked_text_frame() {
        let mut p = FrameParser::default();
        let frame = encode_text_frame(b"world", Some([0x12, 0x34, 0x56, 0x78]));
        let mut got = String::new();
        p.feed(&frame, |_op, payload, _| {
            got = String::from_utf8_lossy(payload).into_owned()
        });
        assert_eq!(got, "world");
    }

    #[test]
    fn handles_frame_split_across_feeds() {
        let mut p = FrameParser::default();
        let frame = encode_text_frame(b"abcdef", None);
        let mut got = Vec::new();
        // 分两次投喂：先头部+部分负载，再剩余。
        p.feed(&frame[..4], |_o, pl, _| {
            got.push(String::from_utf8_lossy(pl).into_owned())
        });
        assert!(got.is_empty(), "未到齐不应 emit");
        p.feed(&frame[4..], |_o, pl, _| {
            got.push(String::from_utf8_lossy(pl).into_owned())
        });
        assert_eq!(got, vec!["abcdef".to_string()]);
    }

    #[test]
    fn reassembles_fragmented_message() {
        let mut p = FrameParser::default();
        // 分片：首帧 text 非 FIN "ab"，续帧 FIN "cd"。
        let first = vec![0x01, 0x02, b'a', b'b']; // opcode text, !fin, len2
        let cont = vec![0x80, 0x02, b'c', b'd']; // opcode cont, fin, len2
        let mut got = Vec::new();
        p.feed(&first, |_o, pl, _| {
            got.push(String::from_utf8_lossy(pl).into_owned())
        });
        assert!(got.is_empty());
        p.feed(&cont, |_o, pl, _| {
            got.push(String::from_utf8_lossy(pl).into_owned())
        });
        assert_eq!(got, vec!["abcd".to_string()]);
    }
}
