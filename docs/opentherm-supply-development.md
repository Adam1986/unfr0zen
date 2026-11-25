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

## Checkpoint: native ABI and dormant builder audit

- The completed CI run [#6](https://github.com/Adam1986/unfr0zen/actions/runs/45630453334) passed all steps, including retained MQTT handling and the existing native builder tests.
- The new ARM harness uses the linked candidate's instructions with mock allocator, constructor, property map, event registration, getter and setter. It compiles locally as a standalone ARM executable. CI now executes it under QEMU; passing that test does not establish the real C++ object's lifecycle or boiler behaviour.
- On the supplied exact-hash binary, a local in-memory audit appended 332 bytes to the existing RX segment, preserved both original hook words, and reproduced the UFP1 journal forward and backward with identical original SHA-256. No firmware image was written.
- Builder tests now exercise the append and rollback primitive with a synthetic ELF fixture using the actual Rust Patch implementation. The helper is test-only and does not alter the production build.
- Native property construction includes C++ cleanup paths; the stock ARM exception index ends at 0x5ab824 (cantunwind), below the appended code. Do not activate the registration hook until unwind coverage, ownership, insertion failure and duplicate-registration semantics are addressed.

## Checkpoint: recovered stock and simpler observer path

The user reports CI for `1fc87cd` passed, including the QEMU mock registration/guard harness and Rust rollback tests.

Recovered the actual stock executable from the earlier 6.4-8 backup by reversing its UFP1 journal. Both backup output hash and restored stock hash were verified. The restored stock SHA-256 is `d28f3e8ec232c4ef122f72ceb1028ddf480a0ad8454677bfc9e818a13cbb7c52` (7262084 bytes), matching the reviewed 2025 golden variant. The supplied installed v0.5.5 binary is different from the earlier backup output, but nine relevant native code/string ranges are byte-identical between recovered stock and v0.5.5. No proprietary binary is committed.

A simpler candidate uses the existing unfr0zen device-commit hook rather than adding a new C++ property. The reviewed bucket globals pointer is at offset 28; native getters read globals+5696 (heating type) and +5716 (supply temperature). The v0.5.5 commit slot at virtual 0x6c7270 has an R_ARM_RELATIVE relocation and currently points to 0x840550.

`opentherm-supply-probe.S` is a read-only candidate, linked offline without unresolved relocations against the exact v0.5.5 addresses. It restores its frame before tail-branching to the previous commit hook; no C++ property allocation or supply setter is called. It would publish a separate atomic diagnostic record containing magic, PID, native configuration and raw 16.16 value. It is NOT installed, connected to production builder or consumed by nestlocal. The record can be stale; it must not be treated as a command acknowledgement.

The new QEMU harness executes the reviewed leaf getter instructions against simulated globals and checks read-only behaviour, file IO failures and original commit return. Actual C++ globals lifetime, thread context, real filesystem behaviour and physical readback are not established by mocks.

Next: validate the observer separately before deciding whether the existing commit hook can support a guarded, one-shot write mechanism. Keep the earlier registration candidate dormant, and do not enable writes or change Phoenix settings.

## Checkpoint: low-limit heating incident and exact-image observer audit

On 25 November 2026, the live Nest was in `BOILER_MODULATION / DRIFT / OFF` and logged `UnexpectedDrift` with active temperature 18.96°C, target 21.5°C and boiler setpoint 10°C. Its physical Pro Setup maximum supply temperature had been left at 20°C during earlier menu exploration. Changing only the physical setting to 50°C immediately restored heating. This is strong evidence of a low-limit operating issue, not evidence of a V6.6 software regression. The exact threshold and other operating limits are not established by this observation. Do not use the full native 0–100°C range as the writable entity's bounds or reset learned parameters. 50°C is the user's current working setting, not a universal recommended minimum.

The standalone offline audit `platform/common/nlclient/tests/opentherm-supply-probe-audit.py` was executed against the supplied exact-hash installed image. It linked the 280-byte observer at the existing executable segment's end and changed only the in-memory device commit slot and two segment size words. The in-memory image SHA-256 was `1e949197385874f968a5fb1e90dfc6a2f3ff510c0c419ba381c7b122f7af12e1` using the local toolchain. Reverse edits restored the original image byte-for-byte; a one-byte modified input was refused. The audit does not save an executable or perform any installation. This separate path is not the production builder. The observer samples before the existing device commit and may be stale until another commit occurs.

Before preparing a read-only device test, confirm the live executable's SHA-256 matches the reviewed image and confirm CI for the observer harness. If hashes differ, re-audit the actual live binary instead of applying the hardcoded offsets. No supply setter or writable MQTT endpoint is enabled.

## Remaining gates before a writable release

1. Review native object ownership, ABI, exceptions and duplicate registration, then validate a disabled-write extension in an isolated ARM environment. Do not install a hook solely on static link evidence.
2. Completed: preserve incoming MQTT RETAIN metadata and reject retained supply commands without disrupting retained weather inputs.
3. Require explicit installation-specific operating limits confirmed against boiler and system documentation; no generic default.
4. Keep all write paths disabled unless both native and bridge validation are in place; independent fresh native observation must confirm any change. A local object-store echo is not confirmation.
5. Exercise native and Rust regressions, prove rollback and test isolated from the live boiler before controlled testing.

No firmware binary, Home Assistant number discovery, or write enablement is included in this branch checkpoint.
