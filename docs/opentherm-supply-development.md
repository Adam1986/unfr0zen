# Experimental OpenTherm maximum supply temperature (V6.7)

This branch starts at V6.6 `c1064a7da9d5ab9efd3a3a86f71e3427bac19679`. It is a development branch, not a release or installation instruction. Maximum supply temperature remains read-only.

## Verified offline on the supplied v0.5.5 binary

SHA-256: `5dbb251b03b1c5f63b1450b680ff6b6ba88eaa5e618ddbec8cf1fdc07bbef567`.

The archived link audit was rerun: 26 static checks and 15 Python model tests passed. These are not execution tests of the native extension. Original firmware remains unchanged. The link candidate in the local development archive is not incorporated into the firmware builder.

- Native supply-temperature property 366, event 77, descriptor 56; standard bucket currently lacks registration and handling.
- Existing DHW property 183, event 82, descriptor 62.
- Native heating-configuration getter reports enum 1 for OpenTherm.
- Native supply temperature is 16.16 fixed point and persisted as supplytemp.
- The physical Nest picker allows 0–100 °C; that is not a validated boiler operating range.
- Phoenix stays disabled. Existing V6.6 maximum-supply readback remains independent of bucket command values.

## First source changes

Named supply writes and raw writes containing the supply property, capability property or readback field are refused through the Bridge command path. A regression test checks no bucket change or notification, and that unrelated raw commands still work. This gate does not cover every possible native or external protocol path. A branch-only CI workflow runs host regression tests; it neither creates a release nor uploads an installable artifact.

## Remaining gates before a writable release

1. Review native object ownership, ABI, exceptions and duplicate registration, then validate a disabled-write extension in an isolated ARM environment. Do not install a hook solely on static link evidence.
2. Preserve incoming MQTT RETAIN metadata and reject retained supply commands without disrupting retained weather inputs.
3. Require explicit installation-specific operating limits confirmed against boiler and system documentation; no generic default.
4. Keep all write paths disabled unless both native and bridge validation are in place; independent fresh native observation must confirm any change. A local object-store echo is not confirmation.
5. Exercise native and Rust regressions, prove rollback and test isolated from the live boiler before controlled testing.

No firmware binary, Home Assistant number discovery, or write enablement is included in this branch checkpoint.
