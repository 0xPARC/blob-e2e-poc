#![allow(clippy::uninlined_format_args)]
//! Run in real mode: `cargo run --release app`
//! Run in mock mode: `cargo run --release app -- --mock`
use std::env;

use app::{DEPTH, Helper, build_predicates};
use pod2::{
    backends::plonky2::{basetypes::DEFAULT_VD_SET, mainpod::Prover, mock::mainpod::MockProver},
    dict,
    frontend::MainPodBuilder,
    middleware::{MainPodProver, Params, VDSet},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = env::args().collect();
    let mock = args.get(1).is_some_and(|arg1| arg1 == "--mock");
    if mock {
        println!("Using MockMainPod")
    } else {
        println!("Using MainPod")
    }

    let mock_prover = MockProver {};
    let real_prover = Prover {};
    let (vd_set, prover): (_, &dyn MainPodProver) = if mock {
        (&VDSet::new(8, &[]).unwrap(), &mock_prover)
    } else {
        println!("Prebuilding circuits to calculate vd_set...");
        let vd_set = &*DEFAULT_VD_SET;
        println!("vd_set calculation complete");
        (vd_set, &real_prover)
    };

    let params = Params::default();
    let predicates = build_predicates(&params);

    // Initial state
    let state = 0;

    // First batch update with 2 updates
    // Update 1: +3
    let op1 = dict!(DEPTH, {"name" => "inc", "n" => 3}).unwrap();

    // Update 2: +4
    let op2 = dict!(DEPTH, {"name" => "inc", "n" => 4}).unwrap();

    let mut builder = MainPodBuilder::new(&params, vd_set);
    let mut helper = Helper::new(&mut builder, &predicates);
    // let mut helper = Helper {
    //     builder: &mut builder,
    //     inc_pred: &inc_pred,
    //     update_pred: &update_pred,
    //     update_loop_pred: &update_loop_pred,
    // };

    let (state, st_update) = helper.st_update(state, &[op1, op2]);
    builder.reveal(&st_update);

    let pod = builder.prove(prover).unwrap();
    println!("# pod\n:{}", pod);
    pod.pod.verify().unwrap();

    // Another batch update with 1 update
    let op3 = dict!(DEPTH, {"name" => "inc", "n" => 1}).unwrap();

    let mut builder = MainPodBuilder::new(&params, vd_set);
    let mut helper = Helper::new(&mut builder, &predicates);

    let (_, st_update) = helper.st_update(state, &[op3]);
    builder.reveal(&st_update);

    let pod = builder.prove(prover).unwrap();
    println!("# pod\n:{}", pod);
    pod.pod.verify().unwrap();

    Ok(())
}
