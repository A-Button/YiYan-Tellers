use criterion::{criterion_group, criterion_main, Criterion};
use rand::rngs::StdRng;
use rand::SeedableRng;

use yyts::{format_sentence, pick_and_format_with_rng, Sentence};

fn sample_sentences() -> Vec<Sentence> {
	vec![
		Sentence {
			front: "人生如梦，何必执着".to_string(),
			behind: "且行且珍惜".to_string(),
			r#type: "quote".to_string(),
			from: "测试来源".to_string(),
			length: 0,
		},
		Sentence {
			front: "天道酬勤".to_string(),
			behind: "自有公论".to_string(),
			r#type: "proverb".to_string(),
			from: "来源2".to_string(),
			length: 0,
		},
	]
}

fn bench_format_sentence(c: &mut Criterion) {
	let s = Sentence {
		front: "这是一个较长的前半句，用来测试字符串拼接性能。".repeat(2),
		behind: "这是一个较长的后半句，用来测试字符串拼接性能。".repeat(2),
		r#type: "t".to_string(),
		from: "bench来源".to_string(),
		length: 0,
	};

	c.bench_function("format_sentence", |b| b.iter(|| {
		let _ = format_sentence(&s);
	}));
}

fn bench_pick_and_format(c: &mut Criterion) {
	let sentences = sample_sentences();

	// 使用确定性 RNG 确保基准的可重复性
	let rng = StdRng::seed_from_u64(42);

	c.bench_function("pick_and_format_with_rng", |b| b.iter(|| {
		// clone RNG to avoid mutation across iterations changing distribution
		let mut local_rng = rng.clone();
		let _ = pick_and_format_with_rng(&sentences, &mut local_rng);
	}));
}

criterion_group!(benches, bench_format_sentence, bench_pick_and_format);
criterion_main!(benches);
