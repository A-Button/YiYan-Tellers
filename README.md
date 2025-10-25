# YiYan-Tellers
yyts：一个简单的终端一言输出实现

<img width="460" height="545" alt="图片" src="https://github.com/user-attachments/assets/8cc874a6-998c-43f1-afda-2126e73ea395" />

## feature
- 从json文件读取
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
