# Counter

A counter that can be incremented by a number smaller than 10 at each update.

## Operations

We don't need an "init" operation because the initial state is `EMPTY_VALUE`
which matches the encoding of `Int(0)`.

```
inc(new, old, op) = AND(
    // Input validation
    DictContains(op, "name", "inc")
    Lt(op.n, 10)
    // State transition
    SumOf(new, old, op.n)
)
```

## Counter View

state at t=0 is `EMPTY_VALUE`

```
update(new, old, op) = OR(
    inc(new, old, op)
)
```

## Commit by state

Just publish a MainPod with one `update` statement.

Then the synchronizer processes updates like this:

```
state[0] = EMPTY_VALUE

i = 1
for pod in blob_pods:
    st = pod.pub_statements[0]
    if !st.pred != update:
        continue # Invalid statement
    if st.args[1] != state[i-1]:
        continue # Invalid old state
    if !verify(pod):
        continue # Invalid proof
    state[i] = st.args[0]
    i += 1
```

In order to have multiple updates in a pod we can adapt the `update` statement
to a batched version, which can be used like a "linked list".

```
update(new, old, private: op) = OR(
    equal(new, old) // base
    update_loop(new, old) // recurse
)

update_batch(new, old, private: int, op) = AND(
    update(int, old)
    inc(new, int, op)
)
```
