#![allow(clippy::uninlined_format_args)]

mod macros;

use std::{collections::HashSet, fmt, str::FromStr, sync::Arc};

use anyhow::{Result, bail};
use common::set_from_value;
use hex::ToHex;
use pod2::{
    frontend::MainPodBuilder,
    lang::parse,
    middleware::{
        CustomPredicateBatch, EMPTY_VALUE, Key, Params, Statement, TypedValue, Value,
        containers::{Dictionary, Set},
    },
};
use serde::{Deserialize, Serialize};

pub const DEPTH: usize = 32;

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
pub fn build_predicates(params: &Params) -> Vec<Arc<CustomPredicateBatch>> {
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
    vec![
        state_batch.clone(),
        rev_state_add_batch.clone(),
        rev_state_del_batch.clone(),
        rev_state_batch.clone(),
    ]
}

pub struct Helper<'a> {
    pub builder: &'a mut MainPodBuilder,
    pub batches: &'a [Arc<CustomPredicateBatch>],
}

impl<'a> Helper<'a> {
    pub fn new(
        pod_builder: &'a mut MainPodBuilder,
        batches: &'a [Arc<CustomPredicateBatch>],
    ) -> Self {
        Self {
            builder: pod_builder,
            batches,
        }
    }

    pub fn st_init(&mut self, old: Dictionary, op: Dictionary) -> Result<(Dictionary, Statement)> {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        assert_eq!(name, "init");
        if Value::from(old.clone()) != Value::from(EMPTY_VALUE) {
            bail!("old state is not empty")
        }
        let init_state = dict!({
            "red" => set!(),
            "green" => set!(),
            "blue" => set!()}
        );

        // init(new, old, op)
        let st = st_custom!(
            (self.builder, self.batches),
            init(
                DictContains(op, "name", "init"),
                Equal(old, EMPTY_VALUE),
                Equal(init_state, init_state),
            )
        );
        Ok((init_state, st))
    }

    pub fn st_add(&mut self, old: Dictionary, op: Dictionary) -> Result<(Dictionary, Statement)> {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        assert!(name == "add");

        let group = Key::try_from(op.get(&Key::from("group")).unwrap().typed()).unwrap();
        let old_group = old.get(&group).unwrap();

        let user = op.get(&Key::from("user")).unwrap();
        let mut new_group = if let TypedValue::Set(set) = old_group.typed() {
            set.clone()
        } else {
            panic!("Value not a Set: {:?}", old_group)
        };
        if new_group.contains(user) {
            bail!("old_group already contains user");
        }
        new_group.insert(user).unwrap();

        let mut new = old.clone();
        new.update(&group, &Value::from(new_group.clone())).unwrap();

        // add(new, old, op, private: old_group, new_group)
        let st = st_custom!(
            (self.builder, self.batches),
            add(
                DictContains(op, "name", "add"),
                DictContains(old, (&op, "group"), old_group),
                SetInsert(new_group, old_group, (&op, "user")),
                DictUpdate(new, old, (&op, "group"), new_group),
            )
        );
        Ok((new, st))
    }

    pub fn st_del(&mut self, old: Dictionary, op: Dictionary) -> Result<(Dictionary, Statement)> {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        assert!(name == "del");

        let group = Key::try_from(op.get(&Key::from("group")).unwrap().typed()).unwrap();
        let old_group = old.get(&group).unwrap();

        let user = op.get(&Key::from("user")).unwrap();
        let mut new_group = if let TypedValue::Set(set) = old_group.typed() {
            set.clone()
        } else {
            panic!("Value not a Set: {:?}", old_group)
        };
        if !new_group.contains(user) {
            bail!("old_group doesn't contain user");
        }
        new_group.delete(user).unwrap();

        let mut new = old.clone();
        new.update(&group, &Value::from(new_group.clone())).unwrap();

        // del(new, old, op, private: old_group, new_group)
        let st = st_custom!(
            (self.builder, self.batches),
            del(
                DictContains(op, "name", "del"),
                DictContains(old, (&op, "group"), old_group),
                SetDelete(new_group, old_group, (&op, "user")),
                DictUpdate(new, old, (&op, "group"), new_group),
            )
        );
        Ok((new, st))
    }

    pub fn st_update(
        &mut self,
        old: Dictionary,
        op: Dictionary,
    ) -> Result<(Dictionary, Statement)> {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        match name.as_str() {
            "init" => {
                // init(new, old, op)
                let (new, st_init) = self.st_init(old, op)?;
                let st = st_custom!(
                    (self.builder, self.batches),
                    update(st_init, Statement::None, Statement::None,)
                );
                Ok((new, st))
            }
            "add" => {
                // add(new, old, op, private: old_group, new_group)
                let (new, st_add) = self.st_add(old, op)?;
                let st = st_custom!(
                    (self.builder, self.batches),
                    update(Statement::None, st_add, Statement::None,)
                );
                Ok((new, st))
            }
            "del" => {
                // del(new, old, op, private: old_group, new_group)
                let (new, st_del) = self.st_del(old, op)?;
                let st = st_custom!(
                    (self.builder, self.batches),
                    update(Statement::None, Statement::None, st_del,)
                );
                Ok((new, st))
            }
            _ => panic!("invalid op.name = {}", name),
        }
    }
}

pub struct RevHelper<'a> {
    pub builder: &'a mut MainPodBuilder,
    pub batches: &'a [Arc<CustomPredicateBatch>],
}

impl<'a> RevHelper<'a> {
    pub fn new(
        pod_builder: &'a mut MainPodBuilder,
        batches: &'a [Arc<CustomPredicateBatch>],
    ) -> Self {
        Self {
            builder: pod_builder,
            batches,
        }
    }

    pub fn st_rev_sync_init(
        &mut self,
        st_update: Statement,
        op: Dictionary,
    ) -> (Dictionary, Statement) {
        let init_rev_state = dict!({});
        let st = st_custom!(
            (self.builder, self.batches),
            rev_sync_init(
                st_update,
                DictContains(op, "name", "init"),
                Equal(init_rev_state, EMPTY_VALUE),
            )
        );
        (init_rev_state, st)
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
        user_groups.insert(group).unwrap();
        let mut new_rev = old_rev.clone();
        new_rev
            .insert(user, &Value::from(user_groups.clone()))
            .unwrap();

        let st = st_custom!(
            (self.builder, self.batches),
            rev_add_fresh(
                SetInsert(user_groups, empty_set, (&op, "group")),
                DictInsert(new_rev, old_rev, (&op, "user"), user_groups),
            )
        );
        (new_rev, st)
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

        let st = st_custom!(
            (self.builder, self.batches),
            rev_add_existing(
                DictContains(old_rev, (&op, "user"), old_user_groups),
                SetInsert(user_groups, old_user_groups, (&op, "group")),
                DictUpdate(new_rev, old_rev, (&op, "user"), user_groups),
            )
        );
        (new_rev, st)
    }

    pub fn st_rev_add(&mut self, old_rev: Dictionary, op: Dictionary) -> (Dictionary, Statement) {
        let user =
            Key::from(String::try_from(op.get(&Key::from("user")).unwrap().typed()).unwrap());
        let group =
            Value::from(String::try_from(op.get(&Key::from("group")).unwrap().typed()).unwrap());
        match old_rev.get(&user) {
            Err(_) => {
                let (new, st_rev_add_fresh) = self.st_rev_add_fresh(old_rev, op, &user, &group);
                let st = st_custom!(
                    (self.builder, self.batches),
                    rev_add(st_rev_add_fresh, Statement::None,)
                );
                (new, st)
            }
            Ok(_) => {
                let (new, st_rev_add_existing) =
                    self.st_rev_add_existing(old_rev, op, &user, &group);
                let st = st_custom!(
                    (self.builder, self.batches),
                    rev_add(Statement::None, st_rev_add_existing,)
                );
                (new, st)
            }
        }
    }

    pub fn st_rev_del_singleton(
        &mut self,
        old_rev: Dictionary,
        op: Dictionary,
        user: &Key,
    ) -> (Dictionary, Statement) {
        let old_user_groups = old_rev.get(user).unwrap();
        let mut new_rev = old_rev.clone();
        new_rev.delete(user).unwrap();

        let st = st_custom!(
            (self.builder, self.batches),
            rev_del_singleton(
                DictContains(old_rev, (&op, "user"), old_user_groups),
                SetDelete(set!(), old_user_groups, (&op, "group")),
                DictDelete(new_rev, old_rev, (&op, "user")),
            )
        );
        (new_rev, st)
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

        let st = st_custom!(
            (self.builder, self.batches),
            rev_del_else(
                DictContains(old_rev, (&op, "user"), old_user_groups),
                SetDelete(user_groups, old_user_groups, (&op, "group")),
                DictUpdate(new_rev, old_rev, (&op, "user"), user_groups),
            )
        );
        (new_rev, st)
    }

    pub fn st_rev_del(&mut self, old_rev: Dictionary, op: Dictionary) -> (Dictionary, Statement) {
        let user =
            Key::from(String::try_from(op.get(&Key::from("user")).unwrap().typed()).unwrap());
        let group =
            Value::from(String::try_from(op.get(&Key::from("group")).unwrap().typed()).unwrap());
        let groups = set_from_value(old_rev.get(&user).unwrap()).unwrap();

        match groups.set().len() {
            1 => {
                if !groups.contains(&group) {
                    panic!("User is not a member of the specified group.")
                }
                let (new, st_rev_del_singleton) = self.st_rev_del_singleton(old_rev, op, &user);
                let st = st_custom!(
                    (self.builder, self.batches),
                    rev_del(st_rev_del_singleton, Statement::None,)
                );
                (new, st)
            }
            _ => {
                let (new, st_rev_del_else) = self.st_rev_del_else(old_rev, op, &user, &group);
                let st = st_custom!(
                    (self.builder, self.batches),
                    rev_del(Statement::None, st_rev_del_else,)
                );
                (new, st)
            }
        }
    }

    pub fn st_rev_sync_add(
        &mut self,
        old_rev: Dictionary,
        st_update: Statement,
        old_st_rev_sync: Statement,
        op: Dictionary,
    ) -> (Dictionary, Statement) {
        let (new, st_rev_add) = self.st_rev_add(old_rev, op.clone());
        let st = st_custom!(
            (self.builder, self.batches),
            rev_sync_add(
                old_st_rev_sync,
                st_update,
                DictContains(op, "name", "add"),
                st_rev_add,
            )
        );
        (new, st)
    }

    pub fn st_rev_sync_del(
        &mut self,
        old_rev: Dictionary,
        st_update: Statement,
        old_st_rev_sync: Statement,
        op: Dictionary,
    ) -> (Dictionary, Statement) {
        let (new, st_rev_del) = self.st_rev_del(old_rev, op.clone());
        let st = st_custom!(
            (self.builder, self.batches),
            rev_sync_del(
                old_st_rev_sync,
                st_update,
                DictContains(op.clone(), "name", "del"),
                st_rev_del,
            )
        );
        (new, st)
    }

    pub fn st_rev_sync(
        &mut self,
        old_rev: Dictionary,
        op: Dictionary,
        st_update: Statement,
        old_st_rev_sync: Statement,
    ) -> (Dictionary, Statement) {
        let name = String::try_from(op.get(&Key::from("name")).unwrap().typed()).unwrap();
        match name.as_str() {
            "init" => {
                // rev_sync_init(rev_state, state)
                let (new, st_rev_sync_init) = self.st_rev_sync_init(st_update, op);
                let st = st_custom!(
                    (self.builder, self.batches),
                    rev_sync(st_rev_sync_init, Statement::None, Statement::None,)
                );
                (new, st)
            }
            "add" => {
                // rev_sync_add(rev_state, state)
                let (new, st_rev_sync_add) =
                    self.st_rev_sync_add(old_rev, st_update, old_st_rev_sync, op);
                let st = st_custom!(
                    (self.builder, self.batches),
                    rev_sync(Statement::None, st_rev_sync_add, Statement::None,)
                );
                (new, st)
            }
            "del" => {
                // rev_sync_del(rev_state, state)
                let (new, st_rev_sync_del) =
                    self.st_rev_sync_del(old_rev, st_update, old_st_rev_sync, op);
                let st = st_custom!(
                    (self.builder, self.batches),
                    rev_sync(Statement::None, Statement::None, st_rev_sync_del,)
                );
                (new, st)
            }
            _ => panic!("invalid op.name = {}", name),
        }
    }
}

#[cfg(test)]
mod tests {
    use pod2::{
        backends::plonky2::mainpod::Prover,
        frontend::{MainPod, MainPodBuilder},
        lang::PrettyPrint,
        middleware::{DEFAULT_VD_SET, MainPodProver, Params, VDSet},
    };

    use super::{Group::*, *};

    #[allow(clippy::too_many_arguments)]
    fn update(
        params: &Params,
        vd_set: &VDSet,
        prover: &dyn MainPodProver,
        batches: &[Arc<CustomPredicateBatch>],
        state: Dictionary,
        rev_state: Dictionary,
        op: Op,
        old_rev_state_pod: Option<MainPod>,
    ) -> (Dictionary, Dictionary, Option<MainPod>) {
        let mut builder = MainPodBuilder::new(params, vd_set);
        let mut helper = Helper::new(&mut builder, batches);

        // State Pod
        let (state, st_update) = helper
            .st_update(state, Dictionary::from(op.clone()))
            .unwrap();
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
        let mut rev_helper = RevHelper::new(&mut builder, batches);
        let (rev_state, rev_st_update) =
            rev_helper.st_rev_sync(rev_state, Dictionary::from(op), st_update, old_st_rev_sync);
        builder.reveal(&rev_st_update);

        let rev_state_pod = builder.prove(prover).unwrap();
        println!("# rev_state_pod\n:{}", rev_state_pod);
        println!(
            "# rev_state\n:{}",
            Value::from(rev_state.clone()).to_podlang_string()
        );
        rev_state_pod.pod.verify().unwrap();

        (state, rev_state, Some(rev_state_pod))
    }

    #[test]
    fn test_app() {
        env_logger::init();
        // let (vd_set, prover) = (
        //     &VDSet::new(8, &[]).unwrap(),
        //     &pod2::backends::plonky2::mock::mainpod::MockProver {},
        // );
        let (vd_set, prover) = (&*DEFAULT_VD_SET, &Prover {});

        let params = Params::default();
        let batches = build_predicates(&params);

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
                &batches,
                state,
                rev_state,
                op,
                rev_state_pod,
            );
        }
    }
}
