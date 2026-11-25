//! Target 1 of DESIGN.md L2129: an Arbitrary-derived sequence of host calls, rendered to a Luau script node
//! and run in a production vehicle VM; every oracle of L2132 is checked by `luau::fuzz::check_calls`.
#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use sim_systems::script::host_api;
use sim_systems::script::luau::fuzz::{check_calls, FuzzArg, FuzzCall};

#[derive(Arbitrary, Debug)]
enum Arg { Nil, Bool(bool), Num(f64), Nan, Inf, NegInf, Huge, NegCount, Int(i32), Vector(f32, f32, f32), Deep(u8), Cyclic, Long(u32), Text(String), Function }

#[derive(Arbitrary, Debug)]
struct Call { func: u16, args: Vec<Arg>, protected: bool, repeat: u16 }

impl From<Arg> for FuzzArg {
    fn from(a: Arg) -> Self {
        match a {
            Arg::Nil => FuzzArg::Nil, Arg::Bool(b) => FuzzArg::Bool(b), Arg::Num(x) => FuzzArg::Num(x), Arg::Nan => FuzzArg::Nan,
            Arg::Inf => FuzzArg::Inf, Arg::NegInf => FuzzArg::NegInf, Arg::Huge => FuzzArg::Huge, Arg::NegCount => FuzzArg::NegCount,
            Arg::Int(i) => FuzzArg::Int(i), Arg::Vector(x, y, z) => FuzzArg::Vector(x, y, z), Arg::Deep(d) => FuzzArg::Deep(d),
            Arg::Cyclic => FuzzArg::Cyclic, Arg::Long(n) => FuzzArg::Long(n), Arg::Text(s) => FuzzArg::Text(s), Arg::Function => FuzzArg::Function,
        }
    }
}

fuzz_target!(|calls: Vec<Call>| {
    // Function ids come from the registry, plus two past its end (a missing global).
    let ids = host_api().len() as u16 + 2;
    let calls: Vec<FuzzCall> = calls.into_iter().take(256).map(|c| FuzzCall {
        func: c.func % ids,
        args: c.args.into_iter().take(8).map(FuzzArg::from).collect(),
        protected: c.protected,
        repeat: c.repeat,
    }).collect();
    if let Err(e) = check_calls(&calls) { panic!("oracle failed: {e}"); }
});
