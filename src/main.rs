use std::error::Error;
use std::io::ErrorKind;

fn main() {
    match run() {
        Ok(()) => {}
        Err(error) => {
            eprintln!("错误: {}", error);
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let corpus = yyts::Corpus::load(&yyts::get_config_path())?;
    if corpus.is_empty() {
        eprintln!("错误: 句子列表为空");
        return Ok(());
    }

    let index = corpus
        .pick_index_fitting(yyts::entropy_seed(), yyts::terminal_width())
        .ok_or("句子索引无效")?;

    // 绝大多数句子（含不可见字符）都放进 8 KiB 栈缓冲，一次 write 输出。
    let mut buffer = [0u8; 8192];
    match corpus.format_into(index, &mut buffer) {
        Some(length) if length < buffer.len() => {
            buffer[length] = b'\n';
            write_stdout(&buffer[..=length]);
        }
        _ => {
            let text = corpus.format_at(index).ok_or("句子索引无效")?;
            write_stdout(text.as_bytes());
            write_stdout(b"\n");
        }
    }

    // 进程马上退出：跳过 munmap，让内核直接回收映射。
    std::mem::forget(corpus);
    Ok(())
}

fn write_stdout(mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: 传入有效切片指针与长度；fd 1 是标准输出。
        let written = unsafe { libc::write(1, bytes.as_ptr().cast(), bytes.len()) };
        if written < 0 {
            if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if written == 0 {
            break;
        }
        bytes = &bytes[written as usize..];
    }
}
