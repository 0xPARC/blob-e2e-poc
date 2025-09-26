#![allow(clippy::uninlined_format_args)]
//! Run in real mode: `cargo run --release app`
//! Run in mock mode: `cargo run --release app -- --mock`

use pod2::{
    frontend::{MainPodBuilder, Operation},
    lang::parse,
    middleware::{
        CustomPredicateRef, Key, Params, Statement,
        containers::{Dictionary, Set},
    },
};

pub const DEPTH: usize = 32;

#[derive(Debug, Clone)]
pub struct Predicates {
    pub ins_pred: CustomPredicateRef,
    pub update_pred: CustomPredicateRef,
    pub update_loop_pred: CustomPredicateRef,
}

pub fn build_predicates(params: &Params) -> Predicates {
    // Operations & Batch View updates
    let input = r#"
        ins(new, old, op) = AND(
            // Input validation
            DictContains(op, "name", "ins")
            // TODO: Data validation
            // State transition
            SetInsert(new, old, op.data)
        )

        update(new, old) = OR(
            Equal(new, old) // base
            update_loop(new, old) // recurse
        )

        update_loop(new, old, private: int, op) = AND(
            update(int, old)
            ins(new, int, op)
        )
    "#;

    let batch = parse(input, params, &[]).unwrap().custom_batch;

    let ins_pred = batch.predicate_ref_by_name("ins").unwrap();
    let update_pred = batch.predicate_ref_by_name("update").unwrap();
    let update_loop_pred = batch.predicate_ref_by_name("update_loop").unwrap();
    Predicates {
        ins_pred,
        update_pred,
        update_loop_pred,
    }
}

pub struct Helper<'a> {
    pub builder: &'a mut MainPodBuilder,
    pub ins_pred: &'a CustomPredicateRef,
    pub update_pred: &'a CustomPredicateRef,
    pub update_loop_pred: &'a CustomPredicateRef,
}

impl<'a> Helper<'a> {
    pub fn new(pod_builder: &'a mut MainPodBuilder, predicates: &'a Predicates) -> Self {
        Self {
            builder: pod_builder,
            ins_pred: &predicates.ins_pred,
            update_pred: &predicates.update_pred,
            update_loop_pred: &predicates.update_loop_pred,
        }
    }
    pub fn st_ins(&mut self, old: Set, op: Dictionary) -> (Set, Statement) {
        let data = op.get(&Key::from("data")).unwrap();
        let mut new = old.clone();
        new.insert(data).unwrap();
        let st0 = self
            .builder
            .priv_op(Operation::dict_contains(op.clone(), "name", "ins"))
            .unwrap();
        let st1 = self
            .builder
            .priv_op(Operation::set_insert(new.clone(), old, (&op, "data")))
            .unwrap();
        (
            new,
            self.builder
                .priv_op(Operation::custom(self.ins_pred.clone(), [st0, st1]))
                .unwrap(),
        )
    }

    pub fn st_update(&mut self, mut old: Set, ops: &[Dictionary]) -> (Set, Statement) {
        let st_none = Statement::None;
        let eq_st = self
            .builder
            .priv_op(Operation::eq(old.clone(), old.clone()))
            .unwrap();
        let mut st_update_prev = self
            .builder
            .priv_op(Operation::custom(
                self.update_pred.clone(),
                [eq_st, st_none.clone()],
            ))
            .unwrap();
        for op in ops {
            let (new, st_ins) = self.st_ins(old, op.clone());
            let st_update_loop = self
                .builder
                .priv_op(Operation::custom(
                    self.update_loop_pred.clone(),
                    [st_update_prev, st_ins],
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
