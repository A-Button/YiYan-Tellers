use rand::rng;

fn main() {
    match yyts::read_sentences() {
        Ok(sentences) => {
            if sentences.is_empty() {
                eprintln!("错误: 句子列表为空");
                return;
            }

            let mut rng = rng();
            if let Some(out) = yyts::pick_and_format_with_rng(&sentences, &mut rng) {
                println!("{}", out);
            }
        }
        Err(e) => {
            eprintln!("错误: {}", e);
            std::process::exit(1);
        }
    }
}
