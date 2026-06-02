use num_traits::Num;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum EloquentConfigValue {
    String(String),
}

/// Configure item types
/// Toggle: Boolean (+xxx, -xxx, key=true, key=1 etc)
/// KeyValue: String (key=value, +key=value, ^key=value)
#[derive(Debug, Clone, PartialEq)]
pub enum EloquentConfigItem {
    // +xxx, -xxx, key=true, key=1
    Toggle {
        key: String,
        enabled: bool,
    },
    // key=value
    KeyValue {
        key: String,
        value: EloquentConfigValue,
        prefix: Option<char>,
    },
}

#[derive(Debug)]
pub struct EloquentConfigParser {
    items: Vec<EloquentConfigItem>,
}

impl EloquentConfigParser {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn parse(&mut self, input: &str) -> Result<(), String> {
        self.items.clear();

        let valid_prefixes = ['+', '-'];

        let mut valid_input = Vec::new();
        for line in input.split("\n") {
            if line.trim().is_empty() {
                continue;
            }

            for x in line.split(" ") {
                if x.is_empty() {
                    continue;
                }

                if x.len() > 1 && valid_prefixes.contains(&x.chars().next().unwrap()) {
                    valid_input.push(x);
                } else if x.contains('=') {
                    valid_input.push(x);
                }
            }
        }

        for item in valid_input {
            let trimmed = item.trim();
            if trimmed.is_empty() {
                continue;
            }

            match self.parse_item(trimmed) {
                Ok(item) => self.items.push(item),
                Err(e) => return Err(format!("Parse error: '{}' - {}", trimmed, e)),
            }
        }

        Ok(())
    }

    fn parse_item(&self, item: &str) -> Result<EloquentConfigItem, String> {
        //Handles +xxx and -xxx formats
        if item.starts_with('+') && !item.contains('=') {
            let key = item[1..].to_string();
            if key.is_empty() {
                return Err("Empty key name".to_string());
            }
            return Ok(EloquentConfigItem::Toggle { key, enabled: true });
        }

        if item.starts_with('-') && !item.contains('=') {
            let key = item[1..].to_string();
            if key.is_empty() {
                return Err("Empty key name".to_string());
            }
            return Ok(EloquentConfigItem::Toggle { key, enabled: false });
        }

        if item.contains('=') {
            return self.parse_key_value(item);
        }

        Err("Unrecognized format".to_string())
    }

    fn parse_key_value(&self, item: &str) -> Result<EloquentConfigItem, String> {
        let mut prefix = None;
        let mut working_item = item;

        if item.starts_with('+') {
            prefix = Some('+');
            working_item = &item[1..];
        } else if item.starts_with('^') {
            prefix = Some('^');
            working_item = &item[1..];
        }

        let parts: Vec<&str> = working_item.splitn(2, '=').collect();
        if parts.len() != 2 {
            return Err("Key-value pairs are formatted incorrectly".to_string());
        }

        let key = parts[0].trim().to_string();
        let value_str = parts[1].trim();

        if key.is_empty() {
            return Err("Empty key name".to_string());
        }

        if self.is_boolean_value(value_str) {
            let enabled = self.parse_boolean(value_str);
            return Ok(EloquentConfigItem::Toggle { key, enabled });
        }

        let value = EloquentConfigValue::String(value_str.to_string());
        Ok(EloquentConfigItem::KeyValue { key, value, prefix })
    }

    fn is_boolean_value(&self, value_str: &str) -> bool {
        matches!(
            value_str.to_lowercase().as_str(),
            "true" | "1" | "yes" | "false" | "0" | "no"
        )
    }

    fn parse_boolean(&self, value_str: &str) -> bool {
        matches!(value_str.to_lowercase().as_str(), "true" | "1" | "yes")
    }

    pub fn get_items(&self) -> &[EloquentConfigItem] {
        &self.items
    }

    /// 转换为HashMap
    pub fn to_hashmap(&self) -> HashMap<String, EloquentConfigValue> {
        let mut map = HashMap::new();

        for item in &self.items {
            match item {
                EloquentConfigItem::Toggle { key, enabled } => {
                    map.insert(key.clone(), EloquentConfigValue::String(enabled.to_string()));
                },
                EloquentConfigItem::KeyValue { key, value, .. } => {
                    map.insert(key.clone(), value.clone());
                },
            }
        }

        map
    }

    /// 获取布尔值
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        for item in &self.items {
            match item {
                EloquentConfigItem::Toggle { key: item_key, enabled } if item_key == key => {
                    return Some(*enabled);
                },
                EloquentConfigItem::KeyValue {
                    key: item_key, value, ..
                } if item_key == key => {
                    return match value {
                        EloquentConfigValue::String(s) => match s.to_lowercase().as_str() {
                            "true" | "1" | "yes" => Some(true),
                            "false" | "0" | "no" => Some(false),
                            _ => None,
                        },
                    };
                },
                _ => {},
            }
        }
        None
    }

    /// 获取字符串值
    pub fn get_string(&self, key: &str) -> Option<String> {
        for item in &self.items {
            match item {
                EloquentConfigItem::Toggle { key: item_key, enabled } if item_key == key => {
                    return Some(enabled.to_string());
                },
                EloquentConfigItem::KeyValue {
                    key: item_key, value, ..
                } if item_key == key => {
                    return match value {
                        EloquentConfigValue::String(s) => Some(s.clone()),
                    };
                },
                _ => {},
            }
        }
        None
    }

    /// 获取任意类型数字
    pub fn get_number<T>(&self, key: &str) -> Option<T>
    where
        T: Num + std::str::FromStr,
    {
        for item in &self.items {
            match item {
                EloquentConfigItem::Toggle { key: item_key, enabled } if item_key == key => {
                    return Some(if *enabled { T::one() } else { T::zero() });
                },
                EloquentConfigItem::KeyValue {
                    key: item_key, value, ..
                } if item_key == key => {
                    return match value {
                        EloquentConfigValue::String(s) => Some(s.parse::<T>().ok()?),
                    };
                },
                _ => {},
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_toggle_parsing() {
        let mut parser = EloquentConfigParser::new();
        parser.parse("+debug\n-verbose").unwrap();

        let items = parser.get_items();
        assert_eq!(items.len(), 2);

        assert_eq!(
            items[0],
            EloquentConfigItem::Toggle {
                key: "debug".to_string(),
                enabled: true
            }
        );

        assert_eq!(
            items[1],
            EloquentConfigItem::Toggle {
                key: "verbose".to_string(),
                enabled: false
            }
        );
    }

    #[test]
    fn test_key_value_parsing() {
        let mut parser = EloquentConfigParser::new();
        parser
            .parse("+timeout=30\nmode=production\n^priority=high\nssl=true\nretry=1\nverbose=yes")
            .unwrap();

        assert_eq!(parser.get_string("timeout").unwrap(), "30");
        assert_eq!(parser.get_number::<u32>("timeout").unwrap(), 30);
        assert_eq!(parser.get_string("mode").unwrap(), "production");
        assert_eq!(parser.get_string("priority").unwrap(), "high");

        assert_eq!(parser.get_bool("ssl").unwrap(), true);
        assert_eq!(parser.get_bool("retry").unwrap(), true);
        assert_eq!(parser.get_number::<u8>("retry").unwrap(), 1);
        assert_eq!(parser.get_number::<f32>("retry").unwrap(), 1.0);
        assert_eq!(parser.get_bool("verbose").unwrap(), true);

        // 验证类型归类
        let items = parser.get_items();
        for item in items {
            match item {
                EloquentConfigItem::Toggle { key, .. } => {
                    assert!(["ssl", "retry", "verbose"].contains(&key.as_str()));
                },
                EloquentConfigItem::KeyValue { key, .. } => {
                    assert!(["timeout", "mode", "priority"].contains(&key.as_str()));
                },
            }
        }
    }

    #[test]
    fn test_mixed_parsing() {
        let mut parser = EloquentConfigParser::new();
        let config = r#"
            +debug
            -cache
            timeout=5000
            +retries=3
            ^database=postgres
            ssl=true
            compress=no
        "#;

        parser.parse(config).unwrap();

        assert_eq!(parser.get_bool("debug").unwrap(), true);
        assert_eq!(parser.get_bool("cache").unwrap(), false);
        assert_eq!(parser.get_string("timeout").unwrap(), "5000");
        assert_eq!(parser.get_string("retries").unwrap(), "3");
        assert_eq!(parser.get_string("database").unwrap(), "postgres");
        assert_eq!(parser.get_bool("ssl").unwrap(), true);
        assert_eq!(parser.get_bool("compress").unwrap(), false);
    }
}
