#![allow(clippy::uninlined_format_args)]

use std::{
    collections::{HashMap, HashSet},
    fmt,
    str::FromStr,
};

use common::set_from_value;
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
    let empty = format!("Raw({:#})", EMPTY_VALUE);
    let empty_state = format!(
        r#"{{"{r}": {empty}, "{g}": {empty}, "{b}": {empty}}}"#,
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
            Equal(old, {empty})
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

        update(new, old, op) = OR(
            init(new, old, op)
            add(new, old, op)
            del(new, old, op)
        )
    "#
    );

    let state_batch = parse(&input_state, params, &[]).unwrap().custom_batch;

    /* NOTE: Wouldn't this be nice?  We commit to the sequence of ops and at the same time allow
     * batching of updates
    let input_state2 = format!(
        r#"
        use _, _, _, update from 0x{state_batch}

        update_rec(new, ops, old_ops, epoch, private: old_old_ops, old_epoch) = AND(
            update2(old, old_ops, old_old_ops, old_epoch)
            SumOf(epoch, old_epoch, 1)
            DictInsert(ops, old_ops, old_epoch, op)
            update(new, old, op)
        )

        update_base(new, ops, old_ops, epoch) = AND(
            Equal(new, {empty})
            Equal(ops, {empty})
            Equal(old_ops, {empty})
            Equal(epoch, 0)
        )

        update2(new, ops, old_ops, epoch) = OR(
            update_base(new, ops, old_ops, epoch)
            update_rec(new, ops, old_ops, epoch)
        )
    "#,
        state_batch = state_batch.id().encode_hex::<String>(),
    );

    let state2_batch = parse(&input_state, params, &[state_batch.clone()])
        .unwrap()
        .custom_batch;
    */

    let input_rev_add = format!(
        r#"
        // Addition
        rev_add_fresh(new, old, op, private: user_groups) = AND(
            SetInsert(user_groups, {empty}, op.group)
            DictInsert(new, old, op.user, user_groups)
        )

        rev_add_existing(new, old, op, private: old_user_groups, user_groups) = AND(
            DictContains(old, op.user, old_user_groups)
            SetInsert(user_groups, old_user_groups, op.group)
            DictUpdate(new, old, op.user, user_groups)
        )

        rev_add(new, old, op) = OR(
            rev_add_fresh(new, old, op)
            rev_add_existing(new, old, op)
        )
    "#
    );

    let rev_state_add_batch = parse(&input_rev_add, params, &[]).unwrap().custom_batch;

    let input_rev_del = format!(
        r#"
        // Deletion
        rev_del_singleton(new, old, op, private: old_user_groups) = AND(
            DictContains(old, op.user, old_user_groups)
            SetDelete({empty}, old_user_groups, op.group)
            DictDelete(new, old, op.user)
        )

        rev_del_else(new, old, op, private: old_user_groups, user_groups) = AND(
            DictContains(old, op.user, old_user_groups)
            SetDelete(user_groups, old_user_groups, op.group)
            DictUpdate(new, old, op.user, user_groups)
        )

        rev_del(new, old, op) = OR(
            rev_del_singleton(new, old, op)
            rev_del_else(new, old, op)
        )
    "#
    );

    let rev_state_del_batch = parse(&input_rev_del, params, &[]).unwrap().custom_batch;

    let input_rev = format!(
        r#"
        use _, _, _, update from 0x{state_batch}
        use _, _, rev_add from 0x{rev_state_add_batch}
        use _, _, rev_del from 0x{rev_state_del_batch}

        // Reverse index & state syncing
        rev_sync_init(rev_state, state, old_state, op) = AND(
            update(state, old_state, op)
            DictContains(op, "name", "init")
            Equal(rev_state, {empty})
        )

        rev_sync_add(rev_state, state, old_state, op, private: old_rev_state) = AND(
            rev_sync(old_rev_state, old_state)
            update(state, old_state, op)
            DictContains(op, "name", "add")
            rev_add(rev_state, old_rev_state, op)
        )

        rev_sync_del(rev_state, state, old_state, op, private: old_rev_state) = AND(
            rev_sync(old_rev_state, old_state)
            update(state, old_state, op)
            DictContains(op, "name", "del")
            rev_del(rev_state, old_rev_state, op)
        )

        rev_sync(rev_state, state, private: old_state, op) = OR(
            rev_sync_init(rev_state, state, old_state, op)
            rev_sync_add(rev_state, state, old_state, op)
            rev_sync_del(rev_state, state, old_state, op)
        )
        "#,
        state_batch = state_batch.id().encode_hex::<String>(),
        rev_state_add_batch = rev_state_add_batch.id().encode_hex::<String>(),
        rev_state_del_batch = rev_state_del_batch.id().encode_hex::<String>(),
    );

    let rev_state_batch = parse(
        &input_rev,
        params,
        &[
            state_batch.clone(),
            rev_state_add_batch.clone(),
            rev_state_del_batch.clone(),
        ],
    )
    .unwrap()
    .custom_batch;

    // State batch predicates

    let state_preds = Predicates {
        init: state_batch.predicate_ref_by_name("init").unwrap(),
        add: state_batch.predicate_ref_by_name("add").unwrap(),
        del: state_batch.predicate_ref_by_name("del").unwrap(),
        update: state_batch.predicate_ref_by_name("update").unwrap(),
    };

    // Reverse index state predicates

    let rev_preds = RevPredicates {
        add_fresh: rev_state_add_batch
            .predicate_ref_by_name("rev_add_fresh")
            .unwrap(),
        add_existing: rev_state_add_batch
            .predicate_ref_by_name("rev_add_existing")
            .unwrap(),
        add: rev_state_add_batch
            .predicate_ref_by_name("rev_add")
            .unwrap(),
        del_singleton: rev_state_del_batch
            .predicate_ref_by_name("rev_del_singleton")
            .unwrap(),
        del_else: rev_state_del_batch
            .predicate_ref_by_name("rev_del_else")
            .unwrap(),
        del: rev_state_del_batch
            .predicate_ref_by_name("rev_del")
            .unwrap(),
        sync_init: rev_state_batch
            .predicate_ref_by_name("rev_sync_init")
            .unwrap(),
        sync_add: rev_state_batch
            .predicate_ref_by_name("rev_sync_add")
            .unwrap(),
        sync_del: rev_state_batch
            .predicate_ref_by_name("rev_sync_del")
            .unwrap(),
        sync: rev_state_batch.predicate_ref_by_name("rev_sync").unwrap(),
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
            // update(new, old, op)
            self.builder
                .priv_op(Operation::custom(self.predicates.update.clone(), sts))
                .unwrap(),
        )
    }

    pub fn st_rev_sync_init(
        &mut self,
        st_update: Statement,
        op: Dictionary,
    ) -> (Dictionary, Statement) {
        let init_rev_state = Dictionary::new(DEPTH, HashMap::new()).unwrap();
        let st1 = self
            .builder
            .priv_op(Operation::dict_contains(op.clone(), "name", "init"))
            .unwrap();
        let st2 = self
            .builder
            .priv_op(Operation::eq(init_rev_state.clone(), EMPTY_VALUE))
            .unwrap();
        (
            init_rev_state,
            self.builder
                .priv_op(Operation::custom(
                    self.rev_predicates.sync_init.clone(),
                    [st_update, st1, st2],
                ))
                .unwrap(),
        )
    }

    pub fn st_rev_add_fresh(
        &mut self,
        old_rev: Dictionary,
        op: Dictionary,
        user: &Key,
        group: &Value,
    ) -> (Dictionary, Statement) {
        let empty_set = Set::new(DEPTH, HashSet::new()).unwrap();
        let mut user_groups = empty_set.clone();
        user_groups.insert(&group).unwrap();
        let mut new_rev = old_rev.clone();
        new_rev
            .insert(user, &Value::from(user_groups.clone()))
            .unwrap();
        let st0 = self
            .builder
            .priv_op(Operation::set_insert(
                user_groups.clone(),
                empty_set,
                (&op, "group"),
            ))
            .unwrap();
        let st1 = self
            .builder
            .priv_op(Operation::dict_insert(
                new_rev.clone(),
                old_rev,
                (&op, "user"),
                user_groups,
            ))
            .unwrap();
        (
            new_rev,
            self.builder
                .priv_op(Operation::custom(
                    self.rev_predicates.add_fresh.clone(),
                    [st0, st1],
                ))
                .unwrap(),
        )
    }

    pub fn st_rev_add_existing(
        &mut self,
        old_rev: Dictionary,
        op: Dictionary,
        user: &Key,
        group: &Value,
    ) -> (Dictionary, Statement) {
        let old_user_groups = old_rev.get(user).unwrap();
        let mut user_groups = if let TypedValue::Set(set) = old_user_groups.typed() {
            set.clone()
        } else {
            panic!("Value not a Set: {:?}", old_user_groups)
        };
        user_groups.insert(group).unwrap();
        let mut new_rev = old_rev.clone();
        new_rev
            .update(user, &Value::from(user_groups.clone()))
            .unwrap();

        let st0 = self
            .builder
            .priv_op(Operation::dict_contains(
                old_rev.clone(),
                (&op, "user"),
                old_user_groups.clone(),
            ))
            .unwrap();
        let st1 = self
            .builder
            .priv_op(Operation::set_insert(
                user_groups.clone(),
                old_user_groups.clone(),
                (&op, "group"),
            ))
            .unwrap();
        let st2 = self
            .builder
            .priv_op(Operation::dict_update(
                new_rev.clone(),
                old_rev,
                (&op, "user"),
                user_groups,
            ))
            .unwrap();
        (
            new_rev,
            self.builder
                .priv_op(Operation::custom(
                    self.rev_predicates.add_existing.clone(),
                    [st0, st1, st2],
                ))
                .unwrap(),
        )
    }

    pub fn st_rev_add(&mut self, old_rev: Dictionary, op: Dictionary) -> (Dictionary, Statement) {
        let user =
            Key::from(String::try_from(op.get(&Key::from("user")).unwrap().typed()).unwrap());
        let group =
            Value::from(String::try_from(op.get(&Key::from("group")).unwrap().typed()).unwrap());
        let st_none = Statement::None;
        let (new, sts) = match old_rev.get(&user) {
            Err(_) => {
                let (new, st) = self.st_rev_add_fresh(old_rev, op, &user, &group);
                (new, [st, st_none])
            }
            Ok(_) => {
                let (new, st) = self.st_rev_add_existing(old_rev, op, &user, &group);
                (new, [st_none, st])
            }
        };
        (
            new,
            self.builder
                .priv_op(Operation::custom(self.rev_predicates.add.clone(), sts))
                .unwrap(),
        )
    }

    pub fn st_rev_del_singleton(
        &mut self,
        old_rev: Dictionary,
        op: Dictionary,
        user: &Key,
    ) -> (Dictionary, Statement) {
        let old_user_groups = old_rev.get(user).unwrap();
        let empty_set = Set::new(DEPTH, HashSet::new()).unwrap();
        let mut new_rev = old_rev.clone();
        new_rev.delete(user).unwrap();

        let st0 = self
            .builder
            .priv_op(Operation::dict_contains(
                old_rev.clone(),
                (&op, "user"),
                old_user_groups.clone(),
            ))
            .unwrap();
        let st1 = self
            .builder
            .priv_op(Operation::set_delete(
                empty_set,
                old_user_groups.clone(),
                (&op, "group"),
            ))
            .unwrap();
        let st2 = self
            .builder
            .priv_op(Operation::dict_delete(
                new_rev.clone(),
                old_rev,
                (&op, "user"),
            ))
            .unwrap();
        (
            new_rev,
            self.builder
                .priv_op(Operation::custom(
                    self.rev_predicates.del_singleton.clone(),
                    [st0, st1, st2],
                ))
                .unwrap(),
        )
    }

    pub fn st_rev_del_else(
        &mut self,
        old_rev: Dictionary,
        op: Dictionary,
        user: &Key,
        group: &Value,
    ) -> (Dictionary, Statement) {
        let old_user_groups = old_rev.get(user).unwrap();
        let mut user_groups = if let TypedValue::Set(set) = old_user_groups.typed() {
            set.clone()
        } else {
            panic!("Value not a Set: {:?}", old_user_groups)
        };
        user_groups.delete(group).unwrap();
        let mut new_rev = old_rev.clone();
        new_rev
            .update(user, &Value::from(user_groups.clone()))
            .unwrap();

        let st0 = self
            .builder
            .priv_op(Operation::dict_contains(
                old_rev.clone(),
                (&op, "user"),
                old_user_groups.clone(),
            ))
            .unwrap();
        let st1 = self
            .builder
            .priv_op(Operation::set_delete(
                user_groups.clone(),
                old_user_groups.clone(),
                (&op, "group"),
            ))
            .unwrap();
        let st2 = self
            .builder
            .priv_op(Operation::dict_update(
                new_rev.clone(),
                old_rev,
                (&op, "user"),
                user_groups,
            ))
            .unwrap();
        (
            new_rev,
            self.builder
                .priv_op(Operation::custom(
                    self.rev_predicates.del_else.clone(),
                    [st0, st1, st2],
                ))
                .unwrap(),
        )
    }

    pub fn st_rev_del(&mut self, old_rev: Dictionary, op: Dictionary) -> (Dictionary, Statement) {
        let user =
            Key::from(String::try_from(op.get(&Key::from("user")).unwrap().typed()).unwrap());
        let group =
            Value::from(String::try_from(op.get(&Key::from("group")).unwrap().typed()).unwrap());
        let st_none = Statement::None;
        let groups = set_from_value(old_rev.get(&user).unwrap()).unwrap();

        let (new, sts) = match groups.set().len() {
            1 => {
                if groups.contains(&group) {
                    let (new, st) = self.st_rev_del_singleton(old_rev, op, &user);
                    (new, [st, st_none])
                } else {
                    panic!("User is not a member of the specified group.")
                }
            }
            _ => {
                let (new, st) = self.st_rev_del_else(old_rev, op, &user, &group);
                (new, [st_none, st])
            }
        };

        (
            new,
            self.builder
                .priv_op(Operation::custom(self.rev_predicates.del.clone(), sts))
                .unwrap(),
        )
    }

    pub fn st_rev_sync_add(
        &mut self,
        old_rev: Dictionary,
        st_update: Statement,
        old_st_rev_sync: Statement,
        op: Dictionary,
    ) -> (Dictionary, Statement) {
        let st2 = self
            .builder
            .priv_op(Operation::dict_contains(op.clone(), "name", "add"))
            .unwrap();
        let (new, st3) = self.st_rev_add(old_rev, op);
        (
            new,
            self.builder
                .priv_op(Operation::custom(
                    self.rev_predicates.sync_add.clone(),
                    [old_st_rev_sync, st_update, st2, st3],
                ))
                .unwrap(),
        )
    }

    pub fn st_rev_sync_del(
        &mut self,
        old_rev: Dictionary,
        st_update: Statement,
        old_st_rev_sync: Statement,
        op: Dictionary,
    ) -> (Dictionary, Statement) {
        let st2 = self
            .builder
            .priv_op(Operation::dict_contains(op.clone(), "name", "del"))
            .unwrap();
        let (new, st3) = self.st_rev_del(old_rev, op);
        (
            new,
            self.builder
                .priv_op(Operation::custom(
                    self.rev_predicates.sync_del.clone(),
                    [old_st_rev_sync, st_update, st2, st3],
                ))
                .unwrap(),
        )
    }

    pub fn st_rev_sync(
        &mut self,
        old_rev: Dictionary,
        op: Dictionary,
        st_update: Statement,
        old_st_rev_sync: Statement,
    ) -> (Dictionary, Statement) {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        let st_none = Statement::None;
        let (new, sts) = match name.as_str() {
            "init" => {
                // rev_sync_init(rev_state, state)
                let (new, st) = self.st_rev_sync_init(st_update, op);
                (new, [st, st_none.clone(), st_none.clone()])
            }
            "add" => {
                // rev_sync_add(rev_state, state)
                let (new, st) = self.st_rev_sync_add(old_rev, st_update, old_st_rev_sync, op);
                (new, [st_none.clone(), st, st_none.clone()])
            }
            "del" => {
                // rev_sync_del(rev_state, state)
                let (new, st) = self.st_rev_sync_del(old_rev, st_update, old_st_rev_sync, op);
                (new, [st_none.clone(), st_none.clone(), st])
            }
            _ => panic!("invalid op.name = {}", name),
        };

        (
            new,
            // rev_sync(rev_state, state)
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
        frontend::{MainPod, MainPodBuilder},
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
        old_rev_state_pod: Option<MainPod>,
    ) -> (Dictionary, Dictionary, Option<MainPod>) {
        let mut builder = MainPodBuilder::new(params, vd_set);
        let mut helper = Helper::new(&mut builder, predicates, rev_predicates);

        // State Pod
        let (state, st_update) = helper.st_update(state, Dictionary::from(op.clone()));
        builder.reveal(&st_update);

        let state_pod = builder.prove(prover).unwrap();
        println!("# state_pod\n:{}", state_pod);
        println!(
            "# state\n:{}",
            Value::from(state.clone()).to_podlang_string()
        );
        state_pod.pod.verify().unwrap();

        // Reverse State Pod
        let mut builder = MainPodBuilder::new(params, vd_set);
        builder.add_pod(state_pod);
        let old_st_rev_sync = if let Some(old_rev_state_pod) = old_rev_state_pod {
            builder.add_pod(old_rev_state_pod.clone());
            old_rev_state_pod.pod.pub_statements()[0].clone()
        } else {
            Statement::None
        };
        let mut helper = Helper::new(&mut builder, predicates, rev_predicates);
        let (rev_state, rev_st_update) =
            helper.st_rev_sync(rev_state, Dictionary::from(op), st_update, old_st_rev_sync);
        builder.reveal(&rev_st_update);

        let rev_state_pod = builder.prove(prover).unwrap();
        println!("# rev_state_pod\n:{}", rev_state_pod);
        println!(
            "# state\n:{}",
            Value::from(state.clone()).to_podlang_string()
        );
        rev_state_pod.pod.verify().unwrap();

        (state, rev_state, Some(rev_state_pod))
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
        let mut rev_state_pod = None;
        for op in [
            Op::Init,
            Op::Add {
                group: Red,
                user: "alice".to_string(),
            },
            Op::Add {
                group: Blue,
                user: "alice".to_string(),
            },
            Op::Add {
                group: Blue,
                user: "bob".to_string(),
            },
            Op::Add {
                group: Red,
                user: "carol".to_string(),
            },
            Op::Del {
                group: Red,
                user: "alice".to_string(),
            },
        ] {
            (state, rev_state, rev_state_pod) = update(
                &params,
                vd_set,
                prover,
                &state_predicates,
                &rev_predicates,
                state,
                rev_state,
                op,
                rev_state_pod,
            );
        }
    }
}
