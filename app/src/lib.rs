#![allow(clippy::uninlined_format_args)]

use std::collections::HashSet;

use pod2::{
    frontend::{MainPodBuilder, Operation},
    lang::parse,
    middleware::{
        CustomPredicateRef, EMPTY_VALUE, Key, Params, Statement, TypedValue, Value,
        containers::{Dictionary, Set},
    },
};

pub const DEPTH: usize = 32;

#[macro_export]
macro_rules! dict {
    ({ $($key:expr => $val:expr),* , }) => (
        $crate::dict!({ $($key => $val),* }).unwrap()
    );
    ({ $($key:expr => $val:expr),* }) => ({
        pod2::dict!(DEPTH, { $($key => $val),* }).unwrap()
    });
}

#[derive(Debug, Clone)]
pub struct Predicates {
    pub init: CustomPredicateRef,
    pub add: CustomPredicateRef,
    pub del: CustomPredicateRef,
    pub update: CustomPredicateRef,
}

pub fn build_predicates(params: &Params) -> Predicates {
    let input = r#"
        init(new, old, op) = AND(
            // Input validation
            DictContains(op, "name", "init")
            // State transition
            Equal(old, EMPTY)
            Equal(new, {"red": EMPTY, "green": EMPTY, "blue": EMPTY})
        )

        add(new, old, op, private: old_group, new_group) = AND(
            // Input validation
            DictContains(op, "name", "add")
            // State transition
            DictContains(old, op.group, old_group)
            SetInsert(new_group, old_group, op.user)
            DictUpdate(new, old, op.group, new_group)
        )

        del(new, old, op, private: old_group, new_group) = AND(
            // Input validation
            DictContains(op, "name", "del")
            // State transition
            DictContains(old, op.group, old_group)
            SetDelete(new_group, old_group, op.user)
            DictUpdate(new, old, op.group, new_group)
        )

        update(new, old, private: op) = OR(
            init(new, old, op)
            add(new, old, op)
            del(new, old, op)
        )
    "#;
    let input = input.replace("EMPTY", &format!("Raw({:#})", EMPTY_VALUE));
    println!("{}", input);

    let batch = parse(&input, params, &[]).unwrap().custom_batch;

    let init_pred = batch.predicate_ref_by_name("init").unwrap();
    let add_pred = batch.predicate_ref_by_name("add").unwrap();
    let del_pred = batch.predicate_ref_by_name("del").unwrap();
    let update_pred = batch.predicate_ref_by_name("update").unwrap();
    Predicates {
        init: init_pred,
        add: add_pred,
        del: del_pred,
        update: update_pred,
    }
}

pub struct Helper<'a> {
    pub builder: &'a mut MainPodBuilder,
    pub predicates: &'a Predicates,
}

impl<'a> Helper<'a> {
    pub fn new(pod_builder: &'a mut MainPodBuilder, predicates: &'a Predicates) -> Self {
        Self {
            builder: pod_builder,
            predicates,
        }
    }

    pub fn st_init(&mut self, old: Dictionary, op: Dictionary) -> (Dictionary, Statement) {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        assert_eq!(name, "init");
        // DictContains(op, "name", "init")
        let st0 = self
            .builder
            .priv_op(Operation::dict_contains(op.clone(), "name", "init"))
            .unwrap();
        // Equal(old, EMPTY)
        let st1 = self
            .builder
            .priv_op(Operation::eq(old.clone(), EMPTY_VALUE))
            .unwrap();

        let empty_group = Value::from(Set::new(DEPTH, HashSet::new()).unwrap());
        let init_state = dict!({
            "red" => empty_group.clone(),
            "green" => empty_group.clone(),
            "blue" => empty_group}
        );
        // Equal(new, {"red": EMPTY, "green": EMPTY, "blue": EMPTY})
        let st2 = self
            .builder
            .priv_op(Operation::eq(init_state.clone(), init_state.clone()))
            .unwrap();

        (
            init_state,
            // init(new, old, op)
            self.builder
                .priv_op(Operation::custom(
                    self.predicates.init.clone(),
                    [st0, st1, st2],
                ))
                .unwrap(),
        )
    }

    pub fn st_add_del(&mut self, old: Dictionary, op: Dictionary) -> (Dictionary, Statement) {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        assert!(name == "add" || name == "del");

        let st0 = if name == "add" {
            // DictContains(op, "name", "add")
            self.builder
                .priv_op(Operation::dict_contains(op.clone(), "name", "add"))
                .unwrap()
        } else {
            // DictContains(op, "name", "del")
            self.builder
                .priv_op(Operation::dict_contains(op.clone(), "name", "del"))
                .unwrap()
        };

        let group = Key::try_from(op.get(&Key::from("group")).unwrap().typed()).unwrap();
        let old_group = old.get(&group).unwrap();
        // DictContains(old, op.group, old_group)
        let st1 = self
            .builder
            .priv_op(Operation::dict_contains(
                old.clone(),
                (&op, "group"),
                old_group.clone(),
            ))
            .unwrap();

        let user = op.get(&Key::from("user")).unwrap();
        let mut new_group = if let TypedValue::Set(set) = old_group.typed() {
            set.clone()
        } else {
            panic!("Value not a Set: {:?}", old_group)
        };
        let st2 = if name == "add" {
            new_group.insert(user).unwrap();
            // SetInsert(new_group, old_group, op.user)
            self.builder
                .priv_op(Operation::set_insert(
                    new_group.clone(),
                    old_group.clone(),
                    (&op, "user"),
                ))
                .unwrap()
        } else {
            new_group.delete(user).unwrap();
            // SetDelete(new_group, old_group, op.user)
            self.builder
                .priv_op(Operation::set_delete(
                    new_group.clone(),
                    old_group.clone(),
                    (&op, "user"),
                ))
                .unwrap()
        };

        let mut new = old.clone();
        new.update(&group, &Value::from(new_group.clone())).unwrap();
        // DictUpdate(new, old, op.group, new_group)
        let st3 = self
            .builder
            .priv_op(Operation::dict_update(
                new.clone(),
                old.clone(),
                (&op, "group"),
                new_group,
            ))
            .unwrap();

        (
            new,
            if name == "add" {
                // add(new, old, op, private: old_group, new_group)
                self.builder
                    .priv_op(Operation::custom(
                        self.predicates.add.clone(),
                        [st0, st1, st2, st3],
                    ))
                    .unwrap()
            } else {
                // del(new, old, op, private: old_group, new_group)
                self.builder
                    .priv_op(Operation::custom(
                        self.predicates.del.clone(),
                        [st0, st1, st2, st3],
                    ))
                    .unwrap()
            },
        )
    }

    pub fn st_update(&mut self, old: Dictionary, op: Dictionary) -> (Dictionary, Statement) {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        let st_none = Statement::None;
        let (new, sts) = match name.as_str() {
            "init" => {
                // init(new, old, op)
                let (new, st) = self.st_init(old, op);
                (new, [st, st_none.clone(), st_none.clone()])
            }
            "add" => {
                // add(new, old, op, private: old_group, new_group)
                let (new, st) = self.st_add_del(old, op);
                (new, [st_none.clone(), st, st_none.clone()])
            }
            "del" => {
                // del(new, old, op, private: old_group, new_group)
                let (new, st) = self.st_add_del(old, op);
                (new, [st_none.clone(), st_none.clone(), st])
            }
            _ => panic!("invalid op.name = {}", name),
        };

        (
            new,
            // update(new, old, private: op)
            self.builder
                .priv_op(Operation::custom(self.predicates.update.clone(), sts))
                .unwrap(),
        )
    }
}

#[cfg(test)]
mod tests {
    use pod2::{
        backends::plonky2::mock::mainpod::MockProver,
        frontend::MainPodBuilder,
        lang::PrettyPrint,
        middleware::{MainPodProver, Params, VDSet},
    };

    use super::*;

    fn update(
        params: &Params,
        vd_set: &VDSet,
        prover: &dyn MainPodProver,
        predicates: &Predicates,
        state: Dictionary,
        op: Dictionary,
    ) -> Dictionary {
        let mut builder = MainPodBuilder::new(&params, vd_set);
        let mut helper = Helper::new(&mut builder, &predicates);

        let (state, st_update) = helper.st_update(state, op);
        builder.reveal(&st_update);

        let pod = builder.prove(prover).unwrap();
        println!("# pod\n:{}", pod);
        println!(
            "# state\n:{}",
            Value::from(state.clone()).to_podlang_string()
        );
        pod.pod.verify().unwrap();

        state
    }

    #[test]
    fn test_app() {
        env_logger::init();
        let (vd_set, prover) = (&VDSet::new(8, &[]).unwrap(), &MockProver {});

        let params = Params::default();
        let predicates = build_predicates(&params);

        // Initial state
        let mut state = dict!({});
        println!(
            "# state\n:{}",
            Value::from(state.clone()).to_podlang_string()
        );

        for op in [
            dict!({"name" => "init"}),
            dict!({"name" => "add", "group" => "red", "user" => "alice"}),
            dict!({"name" => "add", "group" => "blue", "user" => "bob"}),
            dict!({"name" => "add", "group" => "red", "user" => "carol"}),
            dict!({"name" => "del", "group" => "red", "user" => "alice"}),
        ] {
            state = update(&params, &vd_set, prover, &predicates, state, op);
        }
    }
}
