//! 數值節點：在 CPU 上於編譯期求值（每格一次），結果接到其他節點的參數 port。

use crate::{Category, EvalCtx, NodeDef, PortDef, choice, f, int};
use pixelnode_graph::{DataType, Value};

const FLOAT_OUT: &[PortDef] = &[PortDef { name: "值", ty: DataType::Float }];
const COLOR_OUT: &[PortDef] = &[PortDef { name: "顏色", ty: DataType::Color }];

pub(crate) fn all() -> Vec<NodeDef> {
    vec![time(), wave(), random(), math(), make_color()]
}

fn def(
    kind: &'static str,
    title: &'static str,
    outputs: &'static [PortDef],
    params: Vec<crate::ParamDef>,
    time_invariant: bool,
    eval: fn(&[Value], &EvalCtx) -> Value,
) -> NodeDef {
    NodeDef {
        kind,
        title,
        category: Category::Math,
        inputs: &[],
        outputs,
        params,
        per_pixel: true,
        time_invariant,
        cost: 0,
        wgsl: "",
        eval: Some(eval),
    }
}

fn num(v: &Value) -> f32 {
    match *v {
        Value::Float(x) => x,
        Value::Int(i) => i as f32,
        Value::Enum(e) => e as f32,
        Value::Bool(b) => b as u8 as f32,
        _ => 0.0,
    }
}

fn time() -> NodeDef {
    def(
        "time",
        "Time",
        FLOAT_OUT,
        vec![choice("unit", "單位", &["影格", "秒", "循環 0→1"], 2)],
        false,
        |p, cx| {
            let f = cx.frame as f32;
            Value::Float(match num(&p[0]) as i32 {
                0 => f,
                1 => f / cx.fps.max(1) as f32,
                _ => f / cx.frames.max(1) as f32,
            })
        },
    )
}

fn wave() -> NodeDef {
    def(
        "wave",
        "Wave",
        FLOAT_OUT,
        vec![
            f("t", "輸入", -1000.0, 1000.0, 0.0),
            choice("shape", "波形", &["Sine", "Triangle", "Saw", "Square"], 0),
            f("frequency", "頻率", 0.0, 16.0, 1.0),
            f("amplitude", "振幅", 0.0, 10.0, 0.5),
            f("offset", "中心", -10.0, 10.0, 0.5),
            f("phase", "相位", 0.0, 1.0, 0.0),
        ],
        true,
        |p, _| {
            let x = (num(&p[0]) * num(&p[2]) + num(&p[5])).rem_euclid(1.0);
            let w = match num(&p[1]) as i32 {
                0 => (x * std::f32::consts::TAU).sin(),
                1 => 1.0 - 4.0 * (x - 0.5).abs(),
                2 => 2.0 * x - 1.0,
                _ => {
                    if x < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
            };
            Value::Float(num(&p[4]) + num(&p[3]) * w)
        },
    )
}

fn random() -> NodeDef {
    def(
        "random",
        "Random",
        FLOAT_OUT,
        vec![
            f("t", "輸入", -1000.0, 1000.0, 0.0),
            int("seed", "種子", 0, 9999, 0),
            f("min", "最小", -100.0, 100.0, 0.0),
            f("max", "最大", -100.0, 100.0, 1.0),
        ],
        true,
        |p, _| {
            // 依 floor(t) 與種子雜湊：同輸入必得同值（可重現）。
            let mut h = (num(&p[0]).floor() as i64 as u64) ^ ((num(&p[1]) as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
            h ^= h >> 33;
            h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
            h ^= h >> 33;
            let u = (h >> 40) as f32 / (1u64 << 24) as f32;
            Value::Float(num(&p[2]) + (num(&p[3]) - num(&p[2])) * u)
        },
    )
}

fn math() -> NodeDef {
    def(
        "math",
        "Math",
        FLOAT_OUT,
        vec![
            choice("op", "運算", &["a + b", "a − b", "a × b", "a ÷ b", "min", "max", "a ^ b", "a mod b", "|a|", "floor(a)", "sin(a·2π)"], 0),
            f("a", "a", -1000.0, 1000.0, 0.0),
            f("b", "b", -1000.0, 1000.0, 1.0),
        ],
        true,
        |p, _| {
            let (a, b) = (num(&p[1]), num(&p[2]));
            Value::Float(match num(&p[0]) as i32 {
                0 => a + b,
                1 => a - b,
                2 => a * b,
                3 => {
                    if b == 0.0 {
                        0.0
                    } else {
                        a / b
                    }
                }
                4 => a.min(b),
                5 => a.max(b),
                6 => a.powf(b),
                7 => {
                    if b == 0.0 {
                        0.0
                    } else {
                        a.rem_euclid(b)
                    }
                }
                8 => a.abs(),
                9 => a.floor(),
                _ => (a * std::f32::consts::TAU).sin(),
            })
        },
    )
}

fn make_color() -> NodeDef {
    def(
        "make_color",
        "Make Color",
        COLOR_OUT,
        vec![
            f("r", "R", 0.0, 1.0, 1.0),
            f("g", "G", 0.0, 1.0, 1.0),
            f("b", "B", 0.0, 1.0, 1.0),
            f("a", "A", 0.0, 1.0, 1.0),
        ],
        true,
        |p, _| Value::Color([num(&p[0]), num(&p[1]), num(&p[2]), num(&p[3])]),
    )
}

#[cfg(test)]
mod tests {
    use crate::{EvalCtx, find};
    use pixelnode_graph::Value;

    fn run(kind: &str, overrides: &[(usize, Value)], frame: u32) -> Value {
        let d = find(kind).unwrap();
        let mut p: Vec<Value> = d.params.iter().map(|p| p.default.clone()).collect();
        for (i, v) in overrides {
            p[*i] = v.clone();
        }
        (d.eval.unwrap())(&p, &EvalCtx { frame, frames: 10, fps: 5 })
    }

    #[test]
    fn time_units() {
        assert_eq!(run("time", &[(0, Value::Enum(0))], 4), Value::Float(4.0));
        assert_eq!(run("time", &[(0, Value::Enum(1))], 4), Value::Float(0.8));
        assert_eq!(run("time", &[], 5), Value::Float(0.5));
    }

    #[test]
    fn wave_and_math() {
        let Value::Float(v) = run("wave", &[(0, Value::Float(0.25))], 0) else { panic!() };
        assert!((v - 1.0).abs() < 1e-5);
        assert_eq!(run("math", &[(0, Value::Enum(2)), (1, Value::Float(3.0)), (2, Value::Float(4.0))], 0), Value::Float(12.0));
    }

    #[test]
    fn random_is_deterministic_and_in_range() {
        let a = run("random", &[(0, Value::Float(3.0))], 0);
        assert_eq!(a, run("random", &[(0, Value::Float(3.7))], 0));
        assert_ne!(a, run("random", &[(0, Value::Float(4.0))], 0));
        let Value::Float(v) = a else { panic!() };
        assert!((0.0..=1.0).contains(&v));
    }
}
