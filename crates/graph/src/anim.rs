//! 時間軸、keyframe 軌道與插值曲線。

use crate::Value;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    /// 影格數（≥ 1）。
    pub frames: u32,
    pub fps: u32,
}

impl Default for Timeline {
    fn default() -> Self {
        Self { frames: 24, fps: 12 }
    }
}

/// 從此 key 到下一個 key 的插值方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Interp {
    /// 保持數值直到下一個 key（像素動畫常用）。
    Constant,
    #[default]
    Linear,
    /// smoothstep 緩入緩出。
    Smooth,
}

impl Interp {
    pub const ALL: [Interp; 3] = [Interp::Constant, Interp::Linear, Interp::Smooth];

    pub fn label(self) -> &'static str {
        match self {
            Interp::Constant => "常數",
            Interp::Linear => "線性",
            Interp::Smooth => "緩入緩出",
        }
    }

    fn ease(self, t: f32) -> f32 {
        match self {
            Interp::Constant => 0.0,
            Interp::Linear => t,
            Interp::Smooth => t * t * (3.0 - 2.0 * t),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Key {
    pub frame: u32,
    pub value: Value,
    #[serde(default)]
    pub interp: Interp,
}

/// 單一參數的 keyframe 軌道，keys 依 frame 遞增且不重複。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Track {
    pub keys: Vec<Key>,
}

impl Track {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn key_at(&self, frame: u32) -> Option<&Key> {
        self.keys.iter().find(|k| k.frame == frame)
    }

    /// 新增或覆寫某格的 key（覆寫時保留原插值方式）。
    pub fn set_key(&mut self, frame: u32, value: Value) {
        match self.keys.binary_search_by_key(&frame, |k| k.frame) {
            Ok(i) => self.keys[i].value = value,
            Err(i) => {
                let interp = i.checked_sub(1).map_or(Interp::default(), |p| self.keys[p].interp);
                self.keys.insert(i, Key { frame, value, interp });
            }
        }
    }

    pub fn remove_key(&mut self, frame: u32) -> bool {
        let before = self.keys.len();
        self.keys.retain(|k| k.frame != frame);
        self.keys.len() != before
    }

    pub fn sample(&self, frame: u32) -> Option<Value> {
        let first = self.keys.first()?;
        if frame <= first.frame {
            return Some(first.value.clone());
        }
        let i = self.keys.partition_point(|k| k.frame <= frame);
        let a = &self.keys[i - 1];
        let Some(b) = self.keys.get(i) else { return Some(a.value.clone()) };
        let t = (frame - a.frame) as f32 / (b.frame - a.frame) as f32;
        Some(a.value.lerp(&b.value, a.interp.ease(t)))
    }
}

impl Value {
    /// 插值；離散型別（Int 以外的整數語意、Enum、Bool）在 t < 1 時保持 a。
    pub fn lerp(&self, other: &Value, t: f32) -> Value {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        match (self, other) {
            (Value::Float(a), Value::Float(b)) => Value::Float(mix(*a, *b)),
            (Value::Int(a), Value::Int(b)) => Value::Int(mix(*a as f32, *b as f32).round() as i32),
            (Value::Vec2(a), Value::Vec2(b)) => Value::Vec2([mix(a[0], b[0]), mix(a[1], b[1])]),
            (Value::Color(a), Value::Color(b)) => Value::Color(std::array::from_fn(|i| mix(a[i], b[i]))),
            _ if t >= 1.0 => other.clone(),
            _ => self.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Track {
        let mut t = Track::default();
        t.set_key(10, Value::Float(1.0));
        t.set_key(0, Value::Float(0.0));
        t.set_key(20, Value::Float(3.0));
        t
    }

    #[test]
    fn keys_stay_sorted() {
        let frames: Vec<_> = track().keys.iter().map(|k| k.frame).collect();
        assert_eq!(frames, vec![0, 10, 20]);
    }

    #[test]
    fn sampling_clamps_and_interpolates() {
        let mut t = track();
        assert_eq!(t.sample(0), Some(Value::Float(0.0)));
        assert_eq!(t.sample(5), Some(Value::Float(0.5)));
        assert_eq!(t.sample(15), Some(Value::Float(2.0)));
        assert_eq!(t.sample(99), Some(Value::Float(3.0)));
        t.keys[0].interp = Interp::Constant;
        assert_eq!(t.sample(9), Some(Value::Float(0.0)));
        t.keys[0].interp = Interp::Smooth;
        assert_eq!(t.sample(5), Some(Value::Float(0.5)));
        assert!(matches!(t.sample(2), Some(Value::Float(v)) if v < 0.2));
    }

    #[test]
    fn discrete_values_step() {
        let mut t = Track::default();
        t.set_key(0, Value::Enum(0));
        t.set_key(4, Value::Enum(2));
        assert_eq!(t.sample(3), Some(Value::Enum(0)));
        assert_eq!(t.sample(4), Some(Value::Enum(2)));
    }

    #[test]
    fn remove_key() {
        let mut t = track();
        assert!(t.remove_key(10));
        assert!(!t.remove_key(10));
        assert_eq!(t.sample(10), Some(Value::Float(1.5)));
    }
}
