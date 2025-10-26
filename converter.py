import json
import os

def count_separators(text):
    """统计分隔符（，。、；）的数量"""
    separators = "，。、；"
    count = 0
    for char in text:
        if char in separators:
            count += 1
    return count

def split_hitokoto(text):
    """
    将hitokoto文本按中间分隔符分割为front和behind
    分隔符包括：，。、；
    """
    separators = "，。、；"
    
    # 找到所有分隔符的位置
    separator_positions = []
    for i, char in enumerate(text):
        if char in separators:
            separator_positions.append(i)
    
    # 如果没有分隔符或只有一个分隔符，无法分割
    if len(separator_positions) <= 1:
        return text, ""
    
    # 找到中间分隔符的位置
    middle_index = len(separator_positions) // 2
    # 如果分隔符数量为偶数，取前一半的最后一个
    if len(separator_positions) % 2 == 0:
        middle_separator_pos = separator_positions[middle_index - 1]
    else:
        middle_separator_pos = separator_positions[middle_index]
    
    # 分割字符串
    front = text[:middle_separator_pos + 1]  # 包含分隔符
    behind = text[middle_separator_pos + 1:] # 分隔符之后的部分
    
    return front, behind

def process_and_filter_data():
    """处理并过滤JSON数据"""
    
    input_file = input("Filename:")  # 输入文件名
    output_file = "results.json"  # 输出文件名
    
    try:
        # 读取输入JSON文件
        with open(input_file, 'r', encoding='utf-8') as f:
            data = json.load(f)
        
        # 读取已存在的result.json文件（如果存在）
        existing_data = []
        existing_ids = set()
        if os.path.exists(output_file):
            with open(output_file, 'r', encoding='utf-8') as f:
                existing_data = json.load(f)
                # 确保existing_data是列表格式
                if not isinstance(existing_data, list):
                    existing_data = []
                # 获取已存在数据的ID集合，避免重复添加
                existing_ids = {item.get('id', '') for item in existing_data}
        
        # 过滤符合条件的项目并进行字段提取
        processed_items = []
        for item in data:
            # 检查是否已存在
            if item.get('id', '') in existing_ids:
                continue
                
            hitokoto_text = item.get('hitokoto', '')
            length = item.get('length', 0)
            
            # 检查条件：分隔符数量为偶数且length<30
            separator_count = count_separators(hitokoto_text)
            if separator_count % 2 == 0 and length < 30:
                # 分割hitokoto文本
                front, behind = split_hitokoto(hitokoto_text)
                
                # 构建新的项目，只保留指定字段
                new_item = {
                    "id": item.get('id'),  # 保留ID用于去重
                    "front": front,
                    "behind": behind,
                    "type": item.get('type'),
                    "from": item.get('from'),
                    "length": item.get('length')
                }
                
                processed_items.append(new_item)
        
        # 合并新旧数据
        combined_data = existing_data + processed_items
        
        # 写入结果文件
        with open(output_file, 'w', encoding='utf-8') as f:
            json.dump(combined_data, f, ensure_ascii=False, indent=2)
        
        print(f"成功处理数据！")
        print(f"从输入文件找到 {len(processed_items)} 个新符合条件的项目")
        print(f"结果文件现在共有 {len(combined_data)} 个项目")
        
        # 显示几个示例
        if processed_items:
            print("\n新添加的项目示例：")
            for i, example in enumerate(processed_items[:3], 1):
                print(f"示例 {i}:")
                print(f"  id: {example['id']}")
                print(f"  front: {example['front']}")
                print(f"  behind: {example['behind']}")
                print(f"  type: {example['type']}")
                print(f"  from: {example['from']}")
                print(f"  length: {example['length']}")
                print()
        
    except FileNotFoundError:
        print(f"错误：找不到输入文件 '{input_file}'")
    except json.JSONDecodeError:
        print("错误：JSON文件格式不正确")
    except Exception as e:
        print(f"处理过程中出现错误：{e}")

def main():
    """主函数"""
    print("开始处理JSON数据...")
    process_and_filter_data()

if __name__ == "__main__":
    main()
