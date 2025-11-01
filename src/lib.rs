use rand::prelude::IndexedRandom;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct Sentence {
    pub front: String,
    pub behind: String,
    #[allow(dead_code)]
    pub r#type: String,
    #[allow(dead_code)]
    pub from: String,
    #[allow(dead_code)]
    pub length: u32,
}

pub fn get_config_path() -> PathBuf {
    if let Ok(path) = env::var("YYTS_SENTENCES") {
        PathBuf::from(path)
    } else {
        let mut home_dir = dirs::home_dir().expect("无法获取用户主目录");
        home_dir.push(".config");
        home_dir.push("yyts");
        home_dir.push("sentences.json");
        home_dir
    }
}

pub fn read_sentences() -> Result<Vec<Sentence>, Box<dyn std::error::Error>> {
    let config_path = get_config_path();
    let content = fs::read_to_string(&config_path)
        .map_err(|e| format!("无法读取配置文件 {}: {}", config_path.display(), e))?;

    let sentences: Vec<Sentence> = serde_json::from_str(&content)?;
    Ok(sentences)
}

pub fn format_sentence(sentence: &Sentence) -> String {
    let front_len = sentence.front.chars().count();
    let behind_len = sentence.behind.chars().count();
    let max_len = front_len.max(behind_len);

    // 第二行前的空格
    let spaces = "        "; // 8个空格

    // 构建格式化输出
    let mut output = String::new();
    output.push_str("「");
    output.push_str(&sentence.front);
    output.push('\n');
    output.push_str(spaces);
    output.push_str(&sentence.behind);

    // 添加右引号，位置取决于最长行的长度
    let current_line_len = spaces.chars().count() + behind_len;
    let quote_position = max_len + 1; // +1 因为左引号已经占了一个位置

    if current_line_len < quote_position {
        // 当前行不够长，补空格
        let padding = quote_position - current_line_len;
        output.push_str(&" ".repeat(padding));
    }

    output.push_str("」");

    // 添加出处
    output.push_str("  ————《");
    output.push_str(&sentence.from);
    output.push_str("》");
    output
}

/// 从给定的句子列表中随机选择一句并返回格式化后的字符串。
///
/// 该函数接受一个外部提供的 RNG，以便在基准测试中可以使用确定性 RNG（如 StdRng）。
pub fn pick_and_format_with_rng<R: rand::Rng + ?Sized>(sentences: &[Sentence], rng: &mut R) -> Option<String> {
    sentences.choose(rng).map(|s| format_sentence(s))
}
