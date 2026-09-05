# Public release maintenance

The public repository begins with a fresh root commit. Keep private development
history separate: merging or pushing private branches or tags can expose old
files and author metadata even when the current files have been cleaned.
Transfer future changes as reviewed patches and inspect every new file before committing.

This distribution excludes personal profiles, measurement seeds, captured radio
frames, reference screenshots and private infrastructure references. Protocol
fixtures are synthetic; the metrics matrix contains generated parameter combinations.
The example scale address must be replaced locally before capture is used.

Keep credentials, database exports, logs, radio captures and personal configuration
outside version control. Use `config.local.toml` with `--config config.local.toml`
for a local deployment. Ignore rules are a convenience: they do not protect files
already tracked by Git. Review staged changes and commit author/committer metadata
before every public push. Screenshots and release archives need the same review.

The dashboard stores sensitive health data and requires an authenticated reverse
proxy as described in [dashboard deployment](dashboard.md).
