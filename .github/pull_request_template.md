## Summary

Describe the problem and the observable behavior delivered by this pull request.

## Scope

- In scope:
- Out of scope:

## Verification

List commands run and their results. At minimum, include the relevant subset of:

```text
cargo fmt -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
scripts/session-ux-acceptance.sh
```

## Security

- [ ] No control token, `.collab/` artifact, credential, or private project content is included.
- [ ] New inputs and failure modes are documented and tested, or this change does not introduce them.

## Checklist

- [ ] The change has one traceable scope.
- [ ] Documentation and changelog entries are updated when user-visible behavior changes.
- [ ] Tests fail without the implementation and pass with it.
