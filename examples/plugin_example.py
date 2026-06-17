#!/usr/bin/env python3
"""whistle-rs 插件示例（插件即本地 HTTP 服务）。

契约：
- whistle-rs 在命中规则 `plugin://<name>` 时，把请求 POST 到本服务的 `/handle`，
  body 为 JSON：{"method","url","headers":[[k,v]...],"body"}。
- 本服务返回 JSON：{"status"?:u16, "headers"?:[[k,v]...], "body"?:str}，
  whistle-rs 据此直接产生响应（编程式 mock）。

配置（whistle-rs.toml）：
    [plugins]
    myplugin = "127.0.0.1:9300"
规则：
    plugin.test plugin://myplugin

运行：python3 examples/plugin_example.py 9300
"""
import http.server
import json
import sys


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        req = json.loads(self.rfile.read(length) or b"{}")

        # 在此编写你的插件逻辑：检查/改写/mock。
        body = json.dumps(
            {"hello": "from whistle-rs plugin", "you_requested": req.get("url")}
        )
        resp = json.dumps(
            {
                "status": 200,
                "headers": [["content-type", "application/json"], ["x-plugin", "example"]],
                "body": body,
            }
        ).encode()

        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(resp)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(resp)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9300
    http.server.HTTPServer(("127.0.0.1", port), Handler).serve_forever()
