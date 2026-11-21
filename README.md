# YiYan-Tellers
yyts：一个简单的终端一言输出实现

<img width="460" height="545" alt="图片" src="https://github.com/user-attachments/assets/8cc874a6-998c-43f1-afda-2126e73ea395" />

## feature
- 从json文件读取(默认目录为~/.config/yyts/sentences.json，可以通过YYTS_SENTENCES环境变量改变这一行为)
- 按照固定格式输出；没有分隔符的句子按单行排版（`「正文」  ————《出处》`）
- 自动读取终端宽度：抽到的句子放不下就重抽，不会折行（`YYTS_WIDTH` 可指定宽度，`0` 关闭）
- 二进制缓存：JSON 只在缓存缺失时解析一次，之后每次启动只 mmap 一次
- 响应时间约 0.4ms（musl 静态构建）/ 约 0.7ms（默认动态构建）

## 安装
### 从源代码构建
```bash
git clone https://github.com/A-Button/YiYan-Tellers.git
cd ./YiYan-Tellers
cargo build --release
sudo cp ./target/release/yyts /bin
```

### musl 静态构建（启动最快、无动态依赖）
```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
sudo cp ./target/x86_64-unknown-linux-musl/release/yyts /bin
```
仓库里的 `.cargo/config.toml` 已给 musl 目标加上「静态非 PIE」链接参数，比默认的 static-PIE 再快约 6%。

### 从 Releases 安装
```bash
tar -xvf yyts-xxx-x.x.x.tar
sudo cp ./yyts /bin
```

## 性能

`yyts` 是一次性进程，成本大头是启动和解析。JSON 只在缓存缺失时解析一次（冷启动约 7 ms，含启动与写缓存），之后每次启动只 mmap 缓存。

| 场景（10,612 条句库） | 实测 |
| --- | ---: |
| `format_sentence`（双行排版） | 111 ns |
| `format_sentence`（单行句子） | 15 ns |
| 随机选句 + 格式化（`Rng` 接口） | 180 ns |
| 按 80 列过滤选下标（种子路径，不经过 `rand`） | 18 ns |
| 按 80 / 40 / 20 列过滤并格式化 | 141 / 146 / 51 ns |
| 冷启动重建缓存（解析 1.9 MB JSON + 并行算宽度 + 写缓存） | ≈4.2 ms（不含启动） |
| 缓存命中加载（stat + open + mmap + 校验） | 12.3 µs |
| 进程内完整运行一次 | 17.3 µs |
| CLI 端到端 | ≈0.59 ms（glibc）/ ≈0.25 ms（musl 静态） |

端到端受 CPU 调频影响有 ±10% 波动；与上一版在同机交错 A/B 对比，musl 静态快约 12 µs（约 5%）。

缓存位于 `~/.cache/yyts/<hash>.bin`（V4）：64 字节头部 + 每条 24 字节的定长记录（front/behind/from 的偏移与长度）+ n×2 字节的紧凑宽度表 + 连续字符串池。记录按渲染宽度升序存放，抽句时二分出「放得下」的前缀长度 k，再在 `[0, k)` 里均匀抽——不需要重抽，分布和「重抽到放得下为止」等价。头部记录源文件的大小与 mtime，不匹配就自动重建；文件用 mmap 读取，只触碰需要的几页。

- `YYTS_WIDTH=80`：强制按 80 列过滤（默认自动取终端宽度；`0` 关闭过滤）
- `YYTS_SENTENCES=/path`：指定句子库路径
- `YYTS_NO_CACHE=1`：跳过缓存读写（每次解析 JSON，冷路径约 4–7 ms）
- `YYTS_CACHE_DIR=/path/to/dir`：改变缓存目录

重建基准（需要 `~/.config/yyts/sentences.json`，或用 `YYTS_BENCH_DATA` 指定）：

```bash
cargo bench
```

## 示例json文件
```json
[
  {
    "front": "众里寻他千百度，蓦然回首，",
    "behind": "那人却在，灯火阑珊处。",
    "from": "青玉案·元夕",
    "length": 24
  },
  {
    "front": "渔舟唱晚，响穷彭蠡之滨；",
    "behind": "雁阵惊寒，声断衡阳之浦。",
    "from": "滕王阁序",
    "length": 24
  },
  {
    "front": "参差荇菜，左右采之。",
    "behind": "窈窕淑女，琴瑟友之。",
    "from": "关雎",
    "length": 20
  }
]
```
