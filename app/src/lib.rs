#![allow(clippy::uninlined_format_args)]
//! Run in real mode: `cargo run --release app`
//! Run in mock mode: `cargo run --release app -- --mock`

use pod2::{
    frontend::{MainPodBuilder, Operation},
    lang::parse,
    middleware::{CustomPredicateRef, Key, Params, Statement, containers::Dictionary},
};

pub const DEPTH: usize = 32;

#[derive(Debug, Clone)]
pub struct Predicates {
    pub inc_pred: CustomPredicateRef,
    pub update_pred: CustomPredicateRef,
    pub update_loop_pred: CustomPredicateRef,
}

pub fn build_predicates(params: &Params) -> Predicates {
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

    let batch = parse(input, params, &[]).unwrap().custom_batch;

    let inc_pred = batch.predicate_ref_by_name("inc").unwrap();
    let update_pred = batch.predicate_ref_by_name("update").unwrap();
    let update_loop_pred = batch.predicate_ref_by_name("update_loop").unwrap();
    Predicates {
        inc_pred,
        update_pred,
        update_loop_pred,
    }
}

pub struct Helper<'a> {
    pub builder: &'a mut MainPodBuilder,
    pub inc_pred: &'a CustomPredicateRef,
    pub update_pred: &'a CustomPredicateRef,
    pub update_loop_pred: &'a CustomPredicateRef,
}

impl<'a> Helper<'a> {
    pub fn new(pod_builder: &'a mut MainPodBuilder, predicates: &'a Predicates) -> Self {
        Self {
            builder: pod_builder,
            inc_pred: &predicates.inc_pred,
            update_pred: &predicates.update_pred,
            update_loop_pred: &predicates.update_loop_pred,
        }
    }
    pub fn st_inc(&mut self, old: i64, op: Dictionary) -> (i64, Statement) {
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

    pub fn st_update(&mut self, mut old: i64, ops: &[Dictionary]) -> (i64, Statement) {
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
