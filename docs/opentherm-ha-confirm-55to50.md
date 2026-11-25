# Supervised OpenTherm HA confirmation retest (55 C -> 50 C)

Status: offline preparation only; **not yet run on the Nest**. Keep the Nest at 55 C until the operator receives the final run instructions.

The managed supply transaction implementation is based on source commit `78af9620c373e1d0c8cf9be69bb1cab6685487ec`. The staged ARMv7 `nestlocal` candidate is SHA-256 `841c2c9747c3ab39f0e7651db36405f9734acc8d0ab7b0fc4ec77f46ef33d51d`; the staged native `nlclient` candidate is SHA-256 `cde1f5ee4888abdace34b589db74b6b3529de729a8bbba06100cd225e1b743fe`. The custom-screen object was verified byte-identical to the 78af962 CI artifact (SHA-256 `9437430a00090bf8e73fda6219d8cf100e3ae71dbe3c84b87c716574ae56d71c`).

The supervised harness checks the original V6.6 executable hashes, candidate hashes, Monit state, OpenTherm mode, saved 55 C value, fresh idle control line, guard marker and absence of stale command records before it stages either executable. It creates the narrow `55 50` opt-in and expects one Home Assistant submission only. It never creates a second request, retries, or writes a reverse temperature.

## Native claim and confirmation contract

The native worker atomically claims the managed command by renaming `ot-managed.request` to `ot-managed.taken`, and `ot-managed.arm` to `ot-managed.arm.taken`. It then writes `ot-managed.ack`. Consequently, a supervisor must validate the claimed `taken` records after the ACK appears; the original `.request` name is expected to have disappeared. The claimed ARM, request and ACK must share PID and transaction token, and the ACK must report status 2 with raw before/after values 3604480 (55 C) and 3276800 (50 C). The in-flight marker must also have been released.

A matching status-2 ACK means the native setter returned and its immediate native getter matched 50 C. It does not establish a separate Heat Link ACK or independent `settings.config` persistence. The operator must still check the Nest display and heating behaviour after the original V6.6 executables have been restored and their hashes and Monit state verified.

The offline package includes a fixture-driven self-test for the supervisor's ACK parser. The repository contract test separately checks that the Rust/C ABI constants and native claim filenames remain aligned. Neither test simulates a physical thermostat write.
