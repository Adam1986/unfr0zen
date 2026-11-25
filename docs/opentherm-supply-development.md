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

## Live observer test: failed startup, original restored (25 November 2026)

The installed v0.5.5 hash and candidate hash matched the offline audit. The temporary bind-mounted observer failed to complete startup. The activation script's three-second process check incorrectly reported success; subsequent startup attempts logged "client: failed to CreateScreens()" and "MAIN: failed to initialize client globals". Monit logged five restarts within nine cycles and invoked client_recovery at 14:51:31. The reboot removed the temporary bind mount and restored the original executable hash. Native supply stayed at 50°C. No observer record was obtained. Do not re-use the failed activation script.

The equipment-screen and appsetup PNG warnings also occur in successful original V6.6 startup and are not diagnostic of this failure. Offline analysis traced the CreateScreens error to the nonzero return from 0x90e3c (call at 0x99cf0), a layer/screen factory loop returning -1 if a factory returns null. The stock screen registration table starts at 0x6f5568; the installed V6.6 account factory at 0x213b14 branches to custom_factory at 0x84122c. The existing custom_factory opens /var/run/unfr0zen-screen.attempt and returns null if present; this is verified in both the exact installed binary and the V6.6 source. The existing guard is also used on screen entry and first draw. A stale marker could therefore cause a restart loop even before the new device commit observer executes. Its presence during the failure is not established; a reboot clears /var/run. Other factory initialization failures and differences between warm restart and cold boot remain possible.

The observer must remain uninstalled. Investigate the existing factory guard and startup diagnostics offline before any further device experiment. A future device test would require a genuinely completed startup check, immediate rollback on loss of the original PID or initialization failure, a separate test environment where possible, and preservation of Monit/watchdog recovery. Do not remove or bypass the fault guard as a workaround. The previous mocked QEMU tests and successful static audit do not prove live compatibility.

## Control test: unmodified V6.6 warm restart passed

On 25 November, the original installed SHA-256 was confirmed, no temporary executable mount existed, and only /var/run/unfr0zen-screen.done was present. The normal /etc/init.d/nestlabs restart successfully replaced PID 554 with PID 4853 without rebooting the thermostat. The .done marker was refreshed, native control logs resumed and the Heat Link returned ViewConfirm and boiler information. This rules out a general inability to restart V6.6 warm; it does not establish whether the earlier failure was caused by the observer, bind-mounted executable, or a transient startup state. The initial native control line after restart was LEARNING_BAND / HEAT / STAGE_1_HEAT; verify normal OpenTherm modulation once communication has settled before concluding that operation is fully restored.

The original screen fault marker is not proof of the earlier cause. Preserve it and Monit/watchdog recovery. Do not reactivate the failed probe. The next development work is offline review and normal builder integration, with an isolated startup test before another live experiment. No write enablement or functional source change is included in this checkpoint.

## Remaining gates before a writable release

1. Review native object ownership, ABI, exceptions and duplicate registration, then validate a disabled-write extension in an isolated ARM environment. Do not install a hook solely on static link evidence.
2. Completed: preserve incoming MQTT RETAIN metadata and reject retained supply commands without disrupting retained weather inputs.
3. Require explicit installation-specific operating limits confirmed against boiler and system documentation; no generic default.
4. Keep all write paths disabled unless both native and bridge validation are in place; independent fresh native observation must confirm any change. A local object-store echo is not confirmation.
5. Exercise native and Rust regressions, prove rollback and test isolated from the live boiler before controlled testing.

No firmware binary, Home Assistant number discovery, or write enablement is included in this branch checkpoint.


## Live V4 dry-run and V5 supervised write — 25 November 2026

The V4 candidate used the existing keeper's native main-thread event queue,
with the thermostat's original minimal menu profile. The user opened
UNFR0ZEN once after a warm restart to initialise the keeper. Native ACK
status 1 reported 3604480 (55°C) both before and after, for a requested
50°C. No setter was linked into V4. V6.6 was restored successfully.

V5 (build at `aae6af82609e27b5df33b1c03790e697a318aca7`, candidate SHA-256
`f30451a6e4607b16ab03ded57da3f40a3a402f26ee3e94348e7c48f1c6aedfa3`)
used the same event queue with distinct one-shot write authorisation,
restricted to 55°C → 50°C. Heating was idle. Observed:

- Native ACK status 2: 3604480 → 3276800 (55°C → 50°C).
- 19:17:06: Heat Link UpdateRequest, profile 235a000c, tag 16, value 500.
- 19:17:09: Boiler_Setup_Status=1, txn=3. This supports communication
  but is not a transaction-correlated acknowledgement of tag 16.
- The initial persistence check still read 3604480; before original
  restart and after restart the saved value was 3276800.
- V6.6 restoration verified original executable hash, running PID and
  Monit. The user confirmed Home Assistant showed 50°C and then restored
  55°C themselves.

The trial's early persistence timeout was therefore inconclusive at that
instant, not proof of a failed setter. Do not retry the write automatically.
The old observer and registration experiments remain abandoned. The successful
native mechanism is the existing keeper/event queue, not those earlier hooks.

### Managed implementation requirements

Preserve the existing read-only sensor, add a separate non-optimistic MQTT
number only when capability, explicit installation limits and a live native
worker are all established. Keep raw supply writes and retained commands
blocked. Validate command PID/generation, token, bounds, whole degrees and
expected native value; serialise commands. Track native setter return
separately from saved settings and published telemetry; allow delayed
persistence. Timeout or contradictory evidence must be reported as uncertain,
not silently retried or reversed. Never write settings.config directly.
Do not use the representation limit of 100°C as an operating range.

Cold boot and warm restart readiness must be verified independently; the
keeper PID file alone does not prove that pthread creation succeeded. A live
heartbeat is preferable. No release, broad temperature range, or unattended
write has yet been validated. Preserve the stable V6.6 branch and native
screen/watchdog guards.
