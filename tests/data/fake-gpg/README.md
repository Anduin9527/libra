# fake-gpg fixtures

Deterministic OpenPGP material for `plan-20260921` (GnuPG import / signing /
verification). All keys were generated with **gpg 2.4.9** on a scratch
`GNUPGHOME`; no developer key material is involved.

Passphrase for every protected fixture: **`libra-test-fixture-passphrase`**
(also recorded in the plan). Unprotected fixtures use an empty passphrase.

## Files

| File | Primary fingerprint | Primary caps | Purpose |
|---|---|---|---|
| `protected-secret.asc` | `6362FF0BA5456A8E9C7DD8C04FB6368B886D5973` | `scSC` (ed25519) | Protected secret key + signing subkey; the main import round-trip fixture. |
| `pubkey.asc` | `6362FF0BA5456A8E9C7DD8C04FB6368B886D5973` | `scSC` | Matching public key for the protected fixture. |
| `secret-primary-only.asc` | `69AD71C069B596C3AC8B6ED4CDBB1C773B79D67E` | `cC` (certify only) | No signing-capable subkey → selection must fall back to the primary (`primary_key_signs_when_no_usable_signing_subkey`). |
| `secret-encrypt-only-subkey.asc` | `095BE29B5C4263A34FFB7BB41B06C190D0A3B048` | `cEC` + enc-only subkey `96D684119DA1BF1E` | Regression fixture for the key-flags bug: an encryption-only subkey must never be picked as signer (`signing_subkey_requires_sign_flag_and_valid_binding`, `disabled_subkey_is_rejected`). |
| `secret-two-signing-subkeys.asc` | `E3EB58C7B8D61F8F7C4B1AC229A075F24DDBAA3E` | `cSC` + signing subkeys `EC118D168417D1B9` (t=1790125522) and `2522AF449D04EEF9` (t=1790125524) | Newest-valid-subkey selection + determinism (`signing_subkey_selected_by_newest_valid_self_signature`, `subkey_choice_is_deterministic_by_key_id`, `signing_subkey_material_is_used_when_present`). |
| `secret-revoked-key.asc` | `1A388AB7C1C42C1E41D3335E4F52949FAAB33EDD` | revoked (`pub:r:`) | A revoked certificate must not be importable as a signer (`revoked_key_is_rejected_at_selection_time`). |
| `revoked-key.asc` | `1A388AB7C1C42C1E41D3335E4F52949FAAB33EDD` | revoked | Public half of the above. |
| `secret-revoked-subkey.asc` | `C0D870054147472FA9A1B3BD8D2D0A59FBDBC481` | `cC` + **revoked** signing subkey `A7C42C5A0F208C7A` | A revoked signing subkey is skipped, selection falls back to the primary (`revoked_subkey_is_rejected`). |
| `revoked-subkey.asc` | `C0D870054147472FA9A1B3BD8D2D0A59FBDBC481` | `cC` | Public half of the above. |
| `secret-expired-subkey.asc` | `50D6CCE78280BD734923397E631383892167E13C` | `cC` + **expired** signing subkey `A6688F6A48A7839B` (expired 1782525062) | An expired signing subkey is skipped (`expired_subkey_is_rejected_at_evaluation_time`). |
| `expired-subkey.asc` | `50D6CCE78280BD734923397E631383892167E13C` | `cC` | Public half of the above. |

## Regenerating

Use a scratch home so real keys are never touched:

```bash
export GNUPGHOME=$(mktemp -d) && chmod 700 "$GNUPGHOME"
```

Plain keys / subkeys:

```bash
gpg --batch --passphrase '' --quick-generate-key "Name <a@example.invalid>" rsa2048 cert never
gpg --batch --passphrase '' --quick-add-key <FPR> rsa2048 sign never     # or: enc / cert
```

Protected fixture (import/export round-trip): generate as above, then
`--quick-set-passphrase` (or generate with `--passphrase '…'`).

Revoked **key** — `gpg --batch` refuses `--gen-revoke` ("can't do this in batch
mode"), so drive it through a pty:

```bash
printf 'y\n0\n\ny\n' > in.txt
script -qec "gpg --yes --command-fd 0 --gen-revoke <FPR>" /dev/null < in.txt > out.txt
sed -n '/BEGIN PGP/,/END PGP/p' out.txt > rev.asc && gpg --batch --yes --import rev.asc
```

Revoked **subkey** — no `--quick-revoke-subkey` in 2.4.9 and `--quick-revoke-uid`
does not apply, so use the interactive editor through a pty:

```bash
printf 'key 1\nrevkey\ny\n0\n\ny\nsave\n' > in.txt
script -qec "gpg --yes --command-fd 0 --edit-key <FPR>" /dev/null < in.txt
```

Expired **subkey** — key and subkey must both be created in the past, otherwise
gpg reports "Time conflict":

```bash
T1=$(date -d '90 days ago' +%s); T2=$(date -d '89 days ago' +%s)
gpg --batch --passphrase '' --faked-system-time "$T1!" --quick-generate-key "…" rsa2048 cert never
gpg --batch --passphrase '' --faked-system-time "$T2!" --quick-add-key <FPR> rsa2048 sign 1d
```

Export the pair for each fixture (`--armor --export-secret-keys <FPR>` →
`secret-*.asc`, `--armor --export <FPR>` → public name).
