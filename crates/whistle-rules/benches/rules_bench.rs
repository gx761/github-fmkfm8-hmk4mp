//! 规则引擎基准：解析与逐请求匹配的吞吐。
//!
//! 运行：`cargo bench -p whistle-rules`

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use whistle_rules::{MatchInput, RuleSet};

/// 构造一个有一定规模的规则集（域名/通配/正则混合）。
fn sample_rules(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!("host{i}.example.com host://10.0.0.{}\n", i % 255));
        s.push_str(&format!("*.cdn{i}.example.com statusCode://204\n"));
    }
    s.push_str(r"/\.(png|jpe?g|gif)$/ statusCode://404");
    s
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("rules");
    for &n in &[10usize, 100, 1000] {
        let text = sample_rules(n);
        let set = RuleSet::parse(&text).unwrap();
        let input = MatchInput::new("http", "host7.example.com", "/a/b.png");

        group.bench_with_input(BenchmarkId::new("parse", n), &text, |b, t| {
            b.iter(|| RuleSet::parse(criterion::black_box(t)).unwrap())
        });
        group.bench_with_input(BenchmarkId::new("match", n), &set, |b, s| {
            b.iter(|| s.match_request(criterion::black_box(&input)))
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
