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

fn bench_format_single_line(c: &mut Criterion) {
	let s = Sentence {
		front: "孤舟蓑笠翁，独钓寒江雪。".to_string(),
		behind: String::new(),
		r#type: "t".to_string(),
		from: "bench来源".to_string(),
		length: 0,
	};

	c.bench_function("format_sentence_single_line", |b| {
		b.iter(|| {
			let _ = format_sentence(&s);
		})
	});
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

fn bench_data_path() -> std::path::PathBuf {
	if let Ok(path) = std::env::var("YYTS_BENCH_DATA") {
		return path.into();
	}
	std::env::var_os("HOME")
		.map(std::path::PathBuf::from)
		.unwrap_or_else(|| std::path::PathBuf::from("."))
		.join(".config/yyts/sentences.json")
}

fn bench_read_sentences(c: &mut Criterion) {
	let path = bench_data_path();
	if !path.exists() {
		eprintln!("跳过 read_sentences_json：找不到基准数据 {}", path.display());
		return;
	}
	let len = std::fs::metadata(&path).unwrap().len();
	let mut group = c.benchmark_group("json");
	group.throughput(criterion::Throughput::Bytes(len));
	group.bench_function("read_sentences_json", |b| {
		b.iter(|| yyts::read_sentences_from(&path).unwrap())
	});
	group.finish();
}


fn bench_corpus(c: &mut Criterion) {
	let path = bench_data_path();
	if !path.exists() {
		eprintln!("跳过 corpus 基准：找不到基准数据 {}", path.display());
		return;
	}
	let json = std::fs::read(&path).unwrap();

	// 首次从 JSON 建立缓存布局（只有缓存缺失时才会发生）
	let mut group = c.benchmark_group("corpus");
	group.throughput(criterion::Throughput::Bytes(json.len() as u64));
	group.bench_function("parse_json_to_records", |b| {
		b.iter(|| yyts::Corpus::parse(&json).unwrap())
	});

	// 纯选句 + 格式化，不含任何 I/O
	let corpus = yyts::Corpus::parse(&json).unwrap();
	group.throughput(criterion::Throughput::Elements(corpus.len() as u64));
	let rng = StdRng::seed_from_u64(42);
	group.bench_function("pick_and_format", |b| {
		b.iter(|| {
			let mut local_rng = rng.clone();
			let _ = corpus.pick_and_format_with_rng(&mut local_rng);
		})
	});

	// 主路径：种子选下标（不经过 rand）
	group.bench_function("pick_index_fitting_80", |b| {
		b.iter(|| std::hint::black_box(corpus.pick_index_fitting(42, Some(80))))
	});

	// 带终端宽度过滤的选句（80 列，模拟真实提示符）
	group.bench_function("pick_fitting_80", |b| {
		b.iter(|| {
			let mut local_rng = rng.clone();
			let _ = corpus.pick_fitting_with_rng(&mut local_rng, Some(80));
		})
	});
	group.bench_function("pick_fitting_20", |b| {
		b.iter(|| {
			let mut local_rng = rng.clone();
			let _ = corpus.pick_fitting_with_rng(&mut local_rng, Some(20));
		})
	});
	group.bench_function("pick_fitting_40", |b| {
		b.iter(|| {
			let mut local_rng = rng.clone();
			let _ = corpus.pick_fitting_with_rng(&mut local_rng, Some(40));
		})
	});

	// 完整一次运行：定位配置 + 读缓存 + 选句 + 格式化
	group.throughput(criterion::Throughput::Elements(1));
	group.bench_function("run_once", |b| b.iter(|| yyts::run_once().unwrap()));

	// 命中缓存的完整加载路径：stat + open + mmap + 头部校验
	let _ = yyts::Corpus::load(&path).unwrap();
	group.throughput(criterion::Throughput::Bytes(json.len() as u64));
	group.bench_function("load_cached", |b| {
		b.iter(|| yyts::Corpus::load(&path).unwrap())
	});
	group.finish();
}

criterion_group!(
	benches,
	bench_format_sentence,
	bench_format_single_line,
	bench_pick_and_format,
	bench_read_sentences,
	bench_corpus
);
criterion_main!(benches);
