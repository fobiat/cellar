# Facepunch Sandbox profile

Cellar includes a profile for testing a different published s&box game without
touching AppleJackRP:

```powershell
cellar run --config "$env:ProgramData\Cellar\facepunch-sandbox.toml"
```

The checked-in profile is
[configs/facepunch-sandbox.toml](../configs/facepunch-sandbox.toml). Copy it
beside the active Cellar config, then adjust the executable and cache paths if
this machine uses a different s&box installation. It runs the published
`facepunch.sandbox` package, uses its package default map, writes game data and
logs under `C:\AppleJackServer\sbox`, and uses separate ports `27025` and
`27026`.

This profile deliberately disables the AppleJackRP document bridge and
database. Cellar still provides the supervised process, live console, resource
telemetry, logs, addresses, Prometheus metrics, and operator dashboard at
`http://127.0.0.1:8091`.

The dashboard's Configs tab can switch between this profile and the AppleJackRP
profiles. Switching restarts the supervised server, so do it while the test
server is empty.
