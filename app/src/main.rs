//! Run in real mode: `cargo run --release app`
//! Run in mock mode: `cargo run --release app -- --mock`
use std::env;

use pod2::{
    backends::plonky2::{basetypes::DEFAULT_VD_SET, mainpod::Prover, mock::mainpod::MockProver},
    dict,
    frontend::{MainPodBuilder, Operation},
    lang::parse,
    middleware::{
        containers::Dictionary, CustomPredicateRef, Key, MainPodProver, Params, Statement, VDSet,
    },
};

const DEPTH: usize = 32;

struct Helper<'a> {
    builder: &'a mut MainPodBuilder,
    inc_pred: &'a CustomPredicateRef,
    update_pred: &'a CustomPredicateRef,
    update_loop_pred: &'a CustomPredicateRef,
}

impl<'a> Helper<'a> {
    fn st_inc(&mut self, old: i64, op: Dictionary) -> (i64, Statement) {
        let n = i64::try_from(op.get(&Key::from("n")).unwrap().typed()).unwrap();
        let new = old + n;
        let st0 = self
            .builder
            .priv_op(Operation::dict_contains(op.clone(), "name", "inc"))
            .unwrap();
        let st1 = self.builder.priv_op(Operation::lt((&op, "n"), 10)).unwrap();
        let st2 = self
            .builder
            .priv_op(Operation::sum_of(new, old, (&op, "n")))
            .unwrap();
        (
            new,
            self.builder
                .priv_op(Operation::custom(self.inc_pred.clone(), [st0, st1, st2]))
                .unwrap(),
        )
    }

    fn st_update(&mut self, mut old: i64, ops: &[Dictionary]) -> (i64, Statement) {
        let st_none = Statement::None;
        let eq_st = self.builder.priv_op(Operation::eq(old, old)).unwrap();
        let mut st_update_prev = self
            .builder
            .priv_op(Operation::custom(
                self.update_pred.clone(),
                [eq_st, st_none.clone()],
            ))
            .unwrap();
        for op in ops {
            let (new, st_inc) = self.st_inc(old, op.clone());
            let st_update_loop = self
                .builder
                .priv_op(Operation::custom(
                    self.update_loop_pred.clone(),
                    [st_update_prev, st_inc],
                ))
                .unwrap();
            st_update_prev = self
                .builder
                .priv_op(Operation::custom(
                    self.update_pred.clone(),
                    [st_none.clone(), st_update_loop],
                ))
                .unwrap();
            old = new;
        }
        (old, st_update_prev)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = env::args().collect();
    let mock = args.get(1).is_some_and(|arg1| arg1 == "--mock");
    if mock {
        println!("Using MockMainPod")
    } else {
        println!("Using MainPod")
    }

    let params = Params::default();

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

    // Operations & Batch View updates
    let input = r#"
        inc(new, old, op) = AND(
            // Input validation
            DictContains(?op, "name", "inc")
            Lt(?op["n"], 10)
            // State transition
            SumOf(?new, ?old, ?op["n"])
        )

        update(new, old) = OR(
            Equal(?new, ?old) // base
            update_loop(?new, ?old) // recurse
        )

        update_loop(new, old, private: int, op) = AND(
            update(?int, ?old)
            inc(?new, ?int, ?op)
        )
    "#;

    let batch = parse(&input, &params, &[]).unwrap().custom_batch;

    let inc_pred = batch.predicate_ref_by_name("inc").unwrap();
    let update_pred = batch.predicate_ref_by_name("update").unwrap();
    let update_loop_pred = batch.predicate_ref_by_name("update_loop").unwrap();

    // Initial state
    let state = 0;

    // First batch update with 2 updates
    // Update 1: +3
    let op1 = dict!(DEPTH, {"name" => "inc", "n" => 3}).unwrap();

    // Update 2: +4
    let op2 = dict!(DEPTH, {"name" => "inc", "n" => 4}).unwrap();

    let mut builder = MainPodBuilder::new(&params, vd_set);
    let mut helper = Helper {
        builder: &mut builder,
        inc_pred: &inc_pred,
        update_pred: &update_pred,
        update_loop_pred: &update_loop_pred,
    };

    let (state, st_update) = helper.st_update(state, &[op1, op2]);
    builder.reveal(&st_update);

    let pod = builder.prove(prover).unwrap();
    println!("# pod\n:{}", pod);
    pod.pod.verify().unwrap();

    // Another batch update with 1 update
    let op3 = dict!(DEPTH, {"name" => "inc", "n" => 1}).unwrap();

    let mut builder = MainPodBuilder::new(&params, vd_set);
    let mut helper = Helper {
        builder: &mut builder,
        inc_pred: &inc_pred,
        update_pred: &update_pred,
        update_loop_pred: &update_loop_pred,
    };

    let (_, st_update) = helper.st_update(state, &[op3]);
    builder.reveal(&st_update);

    let pod = builder.prove(prover).unwrap();
    println!("# pod\n:{}", pod);
    pod.pod.verify().unwrap();

    Ok(())
}
