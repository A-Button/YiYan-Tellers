# YiYan-Tellers
yyts：一个简单的终端一言输出实现

<img width="460" height="545" alt="图片" src="https://github.com/user-attachments/assets/8cc874a6-998c-43f1-afda-2126e73ea395" />

## feature
- 从json文件读取(默认目录为~/.config/yyts/sentences.json，可以通过YYTS_SENTENCES环境变量改变这一行为)
- 按照固定格式输出
- 响应时间约2ms

## 安装
### 从源代码构建
```bash
git clone https://github.com/A-Button/YiYan-Tellers.git
cd ./YiYan-Tellers
cargo build --release
sudo cp ./target/release/yyts /bin
```

### 从 Releases 安装
```bash
tar -xvf yyts-xxx-x.x.x.tar
sudo cp ./yyts /bin
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
