# docparse-config

Instance-scoped Figment configuration loading, profile/environment overrides,
and typed validation for DocParse. Relative model paths resolve beside the primary
configuration file. Library callers supply configuration explicitly; the CLI does
not search parent directories.

See [configuration and resource limits](../../DEPLOYMENT.md#configuration-and-resource-limits)
([中文](../../DEPLOYMENT.zh-CN.md#配置与资源限制)) and the complete
[docparse.toml](../../docparse.toml) example. Native backend selection belongs to
Cargo features; browser callers supply model artifacts and JavaScript options.
