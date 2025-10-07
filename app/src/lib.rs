#![allow(clippy::uninlined_format_args)]

use std::{
    collections::{HashMap, HashSet},
    fmt,
    str::FromStr,
};

use hex::ToHex;
use pod2::{
    frontend::{MainPodBuilder, Operation},
    lang::parse,
    middleware::{
        CustomPredicateRef, EMPTY_VALUE, Key, Params, Statement, TypedValue, Value,
        containers::{Dictionary, Set},
    },
};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone)]
pub struct RevPredicates {
    pub init: CustomPredicateRef,
    pub add_fresh: CustomPredicateRef,
    pub add_existing: CustomPredicateRef,
    pub add: CustomPredicateRef,
    pub del_singleton: CustomPredicateRef,
    pub del_else: CustomPredicateRef,
    pub del: CustomPredicateRef,
    pub sync_init: CustomPredicateRef,
    pub sync_add: CustomPredicateRef,
    pub sync_del: CustomPredicateRef,
    pub sync: CustomPredicateRef,
}

#[derive(PartialEq, Eq, Hash, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Init,
    Add { group: Group, user: String },
    Del { group: Group, user: String },
}

impl From<Op> for Dictionary {
    fn from(op: Op) -> Self {
        match op {
            Op::Init => dict!({"name" => "init"}),
            Op::Add { group, user } => {
                dict!({"name" => "add", "group" => group, "user" => user})
            }
            Op::Del { group, user } => {
                dict!({"name" => "del", "group" => group, "user" => user})
            }
        }
    }
}

#[derive(PartialEq, Eq, Hash, Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Red = 0,
    Green,
    Blue,
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let str_rep = match self {
            Group::Red => "red",
            Group::Green => "green",
            Group::Blue => "blue",
        };
        write!(f, "{}", str_rep)
    }
}

impl FromStr for Group {
    type Err = Box<dyn std::error::Error>;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "red" => Ok(Self::Red),
            "green" => Ok(Self::Green),
            "blue" => Ok(Self::Blue),
            _ => Err(format!("Invalid index: {}", s).into()),
        }
    }
}

impl From<Group> for TypedValue {
    fn from(val: Group) -> Self {
        format!("{val}").into()
    }
}

/// State = Dict {
///   "red" => Set(...),
///   "green" => Set(...),
///   "blue" => Set(...),
/// }
pub fn build_predicates(params: &Params) -> (Predicates, RevPredicates) {
    let empty_state = format!(
        r#"{{"{r}": EMPTY, "{g}": EMPTY, "{b}": EMPTY}}"#,
        r = Group::Red,
        g = Group::Green,
        b = Group::Blue
    );
    let input_state = format!(
        r#"
        // State predicates
        init(new, old, op) = AND(
            // Input validation
            DictContains(op, "name", "init")
            // State transition
            Equal(old, EMPTY)
            Equal(new, {empty_state})
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
    "#
    );
    let input_state = input_state.replace("EMPTY", &format!("Raw({:#})", EMPTY_VALUE));

    let state_batch = parse(&input_state, params, &[]).unwrap().custom_batch;

    let input_rev = format!(
        r#"
        use init, add, del, _ from 0x{}

        // Reverse index predicates
        rev_state_init(state) = AND(
        Equal(state, EMPTY)
        )

        // Addition
        rev_add_fresh(new, old, x, y, private: s) = AND(
        SetInsert(s, EMPTY, y)
        DictInsert(new, old, x, s)
        )

        rev_add_existing(new, old, x, y, private: old_s, s) = AND(
        DictContains(old, x, old_s)
        SetInsert(s, old_s, y)
        DictUpdate(new, old, x, s)
        )

        rev_add(new, old, x, y) = OR(
        rev_add_fresh(new, old, x, y)
        rev_add_existing(new, old, x, y)
        )

        // Deletion
        rev_del_singleton(new, old, x, y, private: s) = AND(
        DictContains(old, x, s)
        SetInsert(s, EMPTY, y)
        DictDelete(new, old, x)
        )

        rev_del_else(new, old, x, y, private: old_s, s) = AND(
        DictContains(old, x, old_s)
        DictContains(new, x, s)
        SetInsert(s, old_s, y)
        )

        rev_del(new, old, x, y) = OR(
        rev_del_singleton(new, old, x, y)
        rev_del_else(new, old, x, y)
        )

        // Reverse index & state syncing
        is_rev_init(rev_state, state, private: old, op) = AND(
        Equal(rev_state, EMPTY)
        init(state, old, op)
        )

        is_rev_add(rev_state, state, private: op, old_rev_state, old_state, user, group) = AND(
        is_rev(old_rev_state, old_state)
        add(state, old_state, op)
        DictContains(op, "user", user)
        DictContains(op, "group", group)
        rev_add(rev_state, old_rev_state, user, group)
        )

        is_rev_del(rev_state, state, private: op, old_rev_state, old_state, user, group) = AND(
        is_rev(old_rev_state, old_state)
        del(state, old_state, op)
        DictContains(op, "user", user)
        DictContains(op, "group", group)
        rev_del(rev_state, old_rev_state, user, group)
        )

        is_rev(rev_state, state) = OR(
        is_rev_init(rev_state, state)
        is_rev_add(rev_state, state)
        is_rev_del(rev_state, state)
        )
        "#,
        state_batch.id().encode_hex::<String>()
    );
    let input_rev = input_rev.replace("EMPTY", &format!("Raw({:#})", EMPTY_VALUE));

    let rev_state_batch = parse(&input_rev, params, &[state_batch.clone()])
        .unwrap()
        .custom_batch;

    // State batch predicates
    let init_pred = state_batch.predicate_ref_by_name("init").unwrap();
    let add_pred = state_batch.predicate_ref_by_name("add").unwrap();
    let del_pred = state_batch.predicate_ref_by_name("del").unwrap();
    let update_pred = state_batch.predicate_ref_by_name("update").unwrap();

    let state_preds = Predicates {
        init: init_pred,
        add: add_pred,
        del: del_pred,
        update: update_pred,
    };

    // Reverse index state predicates
    let rev_init_pred = rev_state_batch
        .predicate_ref_by_name("rev_state_init")
        .unwrap();
    let rev_add_fresh_pred = rev_state_batch
        .predicate_ref_by_name("rev_add_fresh")
        .unwrap();
    let rev_add_existing_pred = rev_state_batch
        .predicate_ref_by_name("rev_add_existing")
        .unwrap();
    let rev_add_pred = rev_state_batch.predicate_ref_by_name("rev_add").unwrap();
    let rev_del_singleton_pred = rev_state_batch
        .predicate_ref_by_name("rev_del_singleton")
        .unwrap();
    let rev_del_else_pred = rev_state_batch
        .predicate_ref_by_name("rev_del_else")
        .unwrap();
    let rev_del_pred = rev_state_batch.predicate_ref_by_name("rev_del").unwrap();
    let rev_sync_init_pred = rev_state_batch
        .predicate_ref_by_name("is_rev_init")
        .unwrap();
    let rev_sync_add_pred = rev_state_batch.predicate_ref_by_name("is_rev_add").unwrap();
    let rev_sync_del_pred = rev_state_batch.predicate_ref_by_name("is_rev_del").unwrap();
    let rev_sync_pred = rev_state_batch.predicate_ref_by_name("is_rev").unwrap();

    let rev_preds = RevPredicates {
        init: rev_init_pred,
        add_fresh: rev_add_fresh_pred,
        add_existing: rev_add_existing_pred,
        add: rev_add_pred,
        del_singleton: rev_del_singleton_pred,
        del_else: rev_del_else_pred,
        del: rev_del_pred,
        sync_init: rev_sync_init_pred,
        sync_add: rev_sync_add_pred,
        sync_del: rev_sync_del_pred,
        sync: rev_sync_pred,
    };

    (state_preds, rev_preds)
}

pub struct Helper<'a> {
    pub builder: &'a mut MainPodBuilder,
    pub predicates: &'a Predicates,
    pub rev_predicates: &'a RevPredicates,
}

impl<'a> Helper<'a> {
    pub fn new(
        pod_builder: &'a mut MainPodBuilder,
        predicates: &'a Predicates,
        rev_predicates: &'a RevPredicates,
    ) -> Self {
        Self {
            builder: pod_builder,
            predicates,
            rev_predicates,
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

    pub fn rev_st_init(&mut self, state_init_st: Statement) -> (Dictionary, Statement) {
        let st_none = Statement::None;
        let init_rev_state = Dictionary::new(DEPTH, HashMap::new()).unwrap();
        let st0 = self
            .builder
            .priv_op(Operation::eq(init_rev_state.clone(), EMPTY_VALUE))
            .unwrap();
        let st2 = self
            .builder
            .priv_op(Operation::custom(
                self.rev_predicates.sync_init.clone(),
                [st0, state_init_st.clone()],
            ))
            .unwrap();
        (init_rev_state, st2)
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

    pub fn st_update(
        &mut self,
        old: Dictionary,
        op: Dictionary,
    ) -> (Dictionary, Statement, Statement) {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        let st_none = Statement::None;
        let (new, sts, nontrivial_st) = match name.as_str() {
            "init" => {
                // init(new, old, op)
                let (new, st) = self.st_init(old, op);
                (new, [st.clone(), st_none.clone(), st_none.clone()], st)
            }
            "add" => {
                // add(new, old, op, private: old_group, new_group)
                let (new, st) = self.st_add_del(old, op);
                (new, [st_none.clone(), st.clone(), st_none.clone()], st)
            }
            "del" => {
                // del(new, old, op, private: old_group, new_group)
                let (new, st) = self.st_add_del(old, op);
                (new, [st_none.clone(), st_none.clone(), st.clone()], st)
            }
            _ => panic!("invalid op.name = {}", name),
        };

        (
            new,
            // update(new, old, private: op)
            self.builder
                .priv_op(Operation::custom(self.predicates.update.clone(), sts))
                .unwrap(),
            nontrivial_st,
        )
    }

    pub fn rev_st_update(
        &mut self,
        old_rev: Dictionary,
        op: Dictionary,
        state_update_st: Statement,
    ) -> (Dictionary, Statement) {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        let st_none = Statement::None;
        let (new, sts) = match name.as_str() {
            "init" => {
                // init(new, old, op)
                let (new, st) = self.rev_st_init(state_update_st);
                (new, [st, st_none.clone(), st_none.clone()])
            }
            "add" => todo!(),
            "del" => todo!(),
            _ => panic!("invalid op.name = {}", name),
        };

        (
            new,
            // update(new, old, private: op)
            self.builder
                .priv_op(Operation::custom(self.rev_predicates.sync.clone(), sts))
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

    use super::{Group::*, *};

    fn update(
        params: &Params,
        vd_set: &VDSet,
        prover: &dyn MainPodProver,
        predicates: &Predicates,
        rev_predicates: &RevPredicates,
        state: Dictionary,
        rev_state: Dictionary,
        op: Op,
    ) -> (Dictionary, Dictionary) {
        let mut builder = MainPodBuilder::new(params, vd_set);
        let mut helper = Helper::new(&mut builder, predicates, rev_predicates);

        let (state, st_update, st_actual_update) =
            helper.st_update(state, Dictionary::from(op.clone()));
        let (rev_state, rev_st_update) =
            helper.rev_st_update(rev_state, Dictionary::from(op), st_actual_update);
        builder.reveal(&st_update);
        //        builder.reveal(&rev_st_update);

        let pod = builder.prove(prover).unwrap();
        println!("# pod\n:{}", pod);
        println!(
            "# state\n:{}",
            Value::from(state.clone()).to_podlang_string()
        );
        pod.pod.verify().unwrap();

        (state, rev_state)
    }

    #[test]
    fn test_app() {
        env_logger::init();
        let (vd_set, prover) = (&VDSet::new(8, &[]).unwrap(), &MockProver {});

        let params = Params {
            max_custom_batch_size: 20,
            max_custom_predicate_batches: 2,
            ..Params::default()
        };
        let (state_predicates, rev_predicates) = build_predicates(&params);

        // Initial state
        let mut state = dict!({});
        let mut rev_state = dict!({});
        println!(
            "# state\n:{}",
            Value::from(state.clone()).to_podlang_string()
        );

        for op in [
            Op::Init,
            // Op::Add {
            //     group: Red,
            //     user: "alice".to_string(),
            // },
            // Op::Add {
            //     group: Blue,
            //     user: "bob".to_string(),
            // },
            // Op::Add {
            //     group: Red,
            //     user: "carol".to_string(),
            // },
            // Op::Del {
            //     group: Red,
            //     user: "alice".to_string(),
            // },
        ] {
            (state, rev_state) = update(
                &params,
                vd_set,
                prover,
                &state_predicates,
                &rev_predicates,
                state,
                rev_state,
                op,
            );
        }
    }
}
