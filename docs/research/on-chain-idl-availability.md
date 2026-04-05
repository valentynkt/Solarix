# Agent 1B: On-Chain IDL Availability Assessment

## Verdict: PARTIAL

The "give it a program ID, it does everything else" UX story is viable but cannot rely solely on on-chain IDL fetch. On-chain IDLs are available for roughly **50% of the top 100 Solana programs** and only **~20% of the top 1,000**. The good news: nearly all "interesting" DeFi protocols (Jupiter, Raydium, Orca, Meteora, Marinade, Drift) are Anchor-based and have IDLs available either on-chain or via GitHub. The bad news: native Solana programs (SPL Token, System Program) do not use Anchor IDLs, many programs never initialized their IDL account, and on-chain IDLs can be maliciously claimed by attackers on programs that haven't initialized theirs. The recommended UX is a **multi-tier fallback cascade**: try on-chain fetch first, then check the Program Metadata Program, then consult a bundled/curated IDL registry, then offer manual upload.

---

## On-Chain IDL Mechanism

### Storage Method: Anchor IDL Account (Legacy) / Program Metadata Program (New in v1.0)

**Anchor IDL Account (pre-v1.0):**

- Anchor added 7 hidden instructions to every Anchor program to manage IDL upload
- The IDL is stored in a PDA owned by the program itself
- The IDL data is **zlib-compressed** (deflate) to save on-chain space
- The account has fields: `authority` (Pubkey), `data_len` (u32), and `data` (compressed bytes)
- The first 8 bytes of the account data are the standard Anchor discriminator

**Program Metadata Program (PMP) -- New standard as of Anchor v1.0.0:**

- A separate on-chain program (`program-metadata`) that stores IDL and other metadata (security.txt, name, icon) in its own PDAs
- Programs no longer need to embed IDL management instructions in their own binary
- Supports versioned metadata via different seed strings
- Can store canonical (from upgrade authority) and non-canonical (third-party) metadata

### Address Derivation

**Legacy Anchor IDL Account:**

```
PDA = findProgramAddress(["anchor:idl"], programId)
```

Seeds: the literal string `"anchor:idl"` + the program's own ID as the deriving program.

**Program Metadata Program:**

```
PDA = findProgramAddress([<seed>, <program_id>], programMetadataProgramId)
```

Where `<seed>` is a string like `"idl"`, `"security"`, etc.

### Compression

- **Yes, zlib (deflate).** The IDL JSON is compressed before being written on-chain.
- To decode: strip 8-byte discriminator, decode IdlAccount struct, inflate data with zlib, parse resulting JSON.
- The `solana_toolbox_idl` crate depends on the `inflate` crate, confirming it handles decompression.

### Anchor Version Requirements

| Version                   | IDL Behavior                                                                                                              |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| Pre-0.30                  | Legacy IDL format, manual `anchor idl init` required, opt-in                                                              |
| 0.30.0                    | **New IDL spec** -- complete rewrite. Discriminators added. `address` field required. `idl-build` Cargo feature mandatory |
| 0.30.1                    | `anchor idl convert` command added for legacy-to-new conversion                                                           |
| 0.31.0                    | Automatic legacy conversion for most CLI commands (except `idl fetch`)                                                    |
| 0.32.0                    | **IDL upload is now default on `anchor deploy`** (opt-out with `--no-idl`)                                                |
| **1.0.0** (April 2, 2026) | **Legacy IDL instructions REMOVED.** Replaced entirely by Program Metadata Program. Solana SDK 3.x required               |

---

## solana_toolbox_idl Assessment

- **Crate version:** 0.4.5 (with Solana version suffix, e.g., `0.4.5-solana-2.1.4`)
- **Last updated:** December 24, 2024 (last commit on GitHub repo)
- **Total downloads:** ~29,562 (54 versions published)
- **Author:** crypto-vincent
- **License:** MIT
- **Repository:** [github.com/crypto-vincent/solana-toolbox](https://github.com/crypto-vincent/solana-toolbox)
- **Documentation coverage:** 0% on docs.rs (no doc comments)

### API

**Key types:**

- `ToolboxIdlService` -- main entry point. Cached set of `ToolboxIdlProgram` instances. Can parse from JSON string or **fetch from chain** by looking up a program_id's Anchor IDL account.
- `ToolboxIdlProgram` -- represents a single program's IDL. Used for account decoding and instruction encoding.
- `ToolboxIdlInstruction`, `ToolboxIdlAccount`, `ToolboxIdlTypedef` -- IDL structure types.

**Key methods:**

- `get_and_infer_and_decode_account()` -- fetch, resolve, and decode an account
- `get_or_resolve_program()` -- retrieve a `ToolboxIdlProgram` by program ID
- `resolve_and_encode_instruction()` -- generate instruction data from JSON + account addresses

### SDK Compatibility

- Supports both Solana 1.18.x and 2.x via version suffix
- Depends on: `solana-sdk`, `solana_toolbox_endpoint`, `serde_json`, `inflate` (for zlib decompression), `anyhow`, `convert_case`

### Compression Handling

- **Yes.** The `inflate` dependency (^0.4.5) confirms it handles zlib-compressed on-chain IDLs.

### Quality Assessment

- **Moderate risk.** Actively maintained (593 commits), but 0% documentation coverage. Single maintainer. Reasonable download count (~30K) suggests real usage. The version-pinning strategy for Solana SDK compatibility is pragmatic but adds complexity.
- **Critical gap:** As of December 2024, likely does NOT yet support the Program Metadata Program (PMP) introduced in Anchor v1.0.0 (April 2026). Programs deploying with Anchor v1.0+ will store IDLs via PMP, not the legacy Anchor IDL account. This crate may need updates to fetch from PMP.

---

## IDL Availability Matrix

| Program                | Category           | Built w/ Anchor?  | On-Chain IDL?                                 | IDL Version         | Alternative Source                                                                                                                                  |
| ---------------------- | ------------------ | ----------------- | --------------------------------------------- | ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Jupiter v6**         | DEX Aggregator     | Yes               | **Yes** (mainnet)                             | Legacy              | [GitHub IDL](https://github.com/jup-ag/jupiter-cpi/blob/main/idl.json), Solana Explorer                                                             |
| **Raydium CLMM**       | DEX (CLMM)         | Yes (Anchor 0.31) | **Yes** (likely)                              | Legacy              | [GitHub IDL repo](https://github.com/raydium-io/raydium-idl)                                                                                        |
| **Raydium AMM v4**     | DEX (AMM)          | Yes               | **Yes** (likely)                              | Legacy              | GitHub IDL repo                                                                                                                                     |
| **Marinade**           | Liquid Staking     | Yes               | **Yes** (confirmed, `anchor idl fetch` works) | Legacy              | [GitHub source](https://github.com/marinade-finance/liquid-staking-program), [Anchor IDL docs](https://docs.marinade.finance/developers/anchor-idl) |
| **pump.fun**           | Token Launch       | Yes               | **No** (mainnet), Yes (devnet)                | Legacy              | npm `pump-anchor-idl`, community repos                                                                                                              |
| **Orca Whirlpool**     | DEX (CLMM)         | Yes               | **Yes** (confirmed)                           | Legacy              | [dev.orca.so IDL page](https://dev.orca.so/More%20Resources/IDL/), GitHub                                                                           |
| **Meteora DLMM**       | DEX (DLMM)         | Yes               | **Likely Yes**                                | Legacy              | npm `@meteora-ag/dlmm`, [solana-idls repo](https://github.com/tenequm/solana-idls)                                                                  |
| **Tensor Marketplace** | NFT Marketplace    | Yes (Anchor 0.29) | **Likely Yes**                                | Legacy              | [GitHub](https://github.com/tensor-foundation/marketplace), npm                                                                                     |
| **Magic Eden M2**      | NFT Marketplace    | Yes               | **Likely Yes**                                | Legacy              | [AllenHark IDL Library](https://allenhark.com/solana-idl-library)                                                                                   |
| **Drift v2**           | Perpetuals         | Yes               | **Yes**                                       | Legacy              | GitHub, npm                                                                                                                                         |
| **SPL Token**          | Core (Native)      | **No**            | **No** (no Anchor IDL)                        | N/A -- Shank/Codama | Shank-generated IDL, Codama, [solana-program-library](https://github.com/solana-labs/solana-program-library)                                        |
| **Token-2022**         | Core (Native)      | **No**            | **No** (no Anchor IDL)                        | N/A -- Codama       | Codama IDL from [solana-program/token-2022](https://github.com/solana-program/token-2022)                                                           |
| **System Program**     | Core (Native)      | **No**            | **No**                                        | N/A                 | [AllenHark IDL Library](https://allenhark.com/solana-idl-library), solores                                                                          |
| **Metaplex**           | NFT Infrastructure | Yes               | **Yes**                                       | Legacy              | Multiple GitHub repos, Codama                                                                                                                       |
| **Phoenix**            | Order Book DEX     | Yes               | **Likely Yes**                                | Legacy              | GitHub                                                                                                                                              |

---

## Availability Estimate

### Anchor Programs with On-Chain IDL

- **Pre-0.32 era (majority of deployed programs):** IDL upload was **opt-in** (manual `anchor idl init`). Many programs never bothered. Estimate: **40-60%** of Anchor programs have on-chain IDLs.
- **Post-0.32 / v1.0 era (new deployments):** IDL upload is **default on deploy**. Nearly all new Anchor programs will have IDLs. Estimate: **90%+** going forward.
- **Overall currently:** ~50% of top 100 programs, ~20% of top 1,000 (per published statistics).

### Non-Anchor Programs

- Native Solana programs (SPL Token, System, Stake, Vote, Compute Budget) do **NOT** have Anchor IDL accounts.
- Shank and Codama can generate IDLs for these, but the IDLs are not on-chain in the Anchor format.
- The `solana-include-idl` crate embeds IDLs in the program binary ELF section, but this is a newer approach with limited adoption.
- Effectively: non-Anchor programs require curated/bundled IDLs.

### Overall "Give It a Program ID" Success Rate

- **For top DeFi protocols (top ~50):** ~70-80% success via on-chain fetch alone
- **For broader ecosystem (top 1,000):** ~20-30% via on-chain fetch alone
- **With multi-tier fallback (on-chain + curated registry + GitHub):** ~85-95% for "interesting" programs
- **For arbitrary unknown programs:** ~30-40% (many small programs never publish IDLs)

---

## Recommended UX Flow

Based on findings, the user experience should implement a **priority-ordered fallback cascade:**

```
1. Try on-chain IDL fetch (Anchor IDL account at PDA["anchor:idl", programId])
   - Handles: Most Anchor programs that initialized their IDL
   - Time: ~1 RPC call

2. Try Program Metadata Program (PMP) fetch
   - Handles: Anchor v1.0+ programs, any program using PMP
   - Time: ~1 RPC call
   - NOTE: This is the new standard as of April 2026

3. Check solana-include-idl ELF section (if program binary is available)
   - Handles: Programs embedding IDL in their binary
   - Time: Requires downloading program binary

4. Consult bundled/curated IDL registry
   - Ship Solarix with a curated set of IDLs for top protocols
   - Sources: AllenHark library (70+ IDLs), DeployDAO program index, Helius Orb tags
   - Include: SPL Token, Token-2022, System, Stake, all top DeFi protocols
   - This is the KEY fallback for native programs and programs without on-chain IDLs

5. Check known GitHub repositories (optional, online-only)
   - Fetch from known org repos (jup-ag, raydium-io, orca-so, etc.)
   - Could use a registry mapping program IDs to GitHub IDL URLs

6. Manual JSON file upload (last resort)
   - User provides IDL file from their local machine
   - Clear UX: "We couldn't find an IDL for this program. Do you have one?"
```

### UX Messaging Recommendation

When step 1-2 succeed: "IDL found on-chain. Ready to index."
When step 3-4 succeed: "IDL found in registry. Ready to index."
When all auto-steps fail: "No IDL found for program [ID]. You can provide one manually, or check [link to IDL sources]."

---

## Anchor v1.0 Impact

### Release Date

April 2, 2026 (3 days ago as of this report)

### IDL Format Changes

- The IDL **spec itself** was stabilized in v0.30.0 (April 2024) and has been stable since.
- No further format changes in v1.0.0 -- the IDL JSON structure remains the same as v0.30+.
- Key fields: `address` (program ID), `metadata.spec` (IDL spec version), `instructions[]` with discriminators, `accounts[]`, `types[]`, `events[]`, `errors[]`.

### Storage Changes -- CRITICAL

- **The legacy on-chain IDL management instructions have been REMOVED in v1.0.0.**
- Replaced entirely by the **Program Metadata Program (PMP)**.
- `anchor deploy` and `anchor idl` commands work the same way from the CLI perspective, but the underlying storage mechanism changed.
- Old programs with legacy Anchor IDL accounts: **their existing IDL accounts remain readable.** They are not deleted. But new uploads from v1.0 will go to PMP.

### Backward Compatibility Assessment

- **Reading legacy IDLs:** Still works. The PDA at `["anchor:idl"]` still exists for old programs.
- **Reading v0.30+ vs pre-0.30 IDLs:** Two different JSON formats. `anchor idl convert` can translate legacy to new.
- **Reading v1.0 IDLs:** Must query the Program Metadata Program instead of the legacy PDA.
- **Impact on Solarix:** Must support BOTH fetch paths:
  1. Legacy: `PDA(["anchor:idl"], programId)` for pre-v1.0 programs
  2. New: `PDA(["idl", programId], programMetadataProgramId)` for v1.0+ programs
- **Impact on `solana_toolbox_idl`:** The crate (last updated Dec 2024) likely does NOT yet support PMP fetching. This is a risk factor -- may need to implement PMP fetch ourselves or wait for a crate update.

### TypeScript Package Migration

- `@coral-xyz/anchor` is now `@anchor-lang/core` (matters for any TS tooling in Solarix)

---

## Alternative IDL Sources (Detailed)

| Source                           | Type                | Coverage                  | Notes                                                        |
| -------------------------------- | ------------------- | ------------------------- | ------------------------------------------------------------ |
| **On-chain Anchor IDL account**  | Automatic           | ~20-50% of programs       | Legacy mechanism, still readable                             |
| **Program Metadata Program**     | Automatic           | Growing (v1.0+ programs)  | New standard as of April 2026                                |
| **AllenHark Solana IDL Library** | Curated             | 70+ IDLs, 32+ protocols   | Free download, covers all major DeFi + NFT + native programs |
| **DeployDAO Program Index**      | Verified            | Unknown count             | Verified builds + IDLs, GitHub flat-file API                 |
| **Helius Orb**                   | Explorer            | 2,000+ tagged programs    | Web UI, may not expose raw IDL API                           |
| **Shyft SuperIndexers**          | API Service         | Depends on submitted IDLs | Given an IDL, spins up GraphQL API                           |
| **`solana-idls` npm (tenequm)**  | Package             | 41+ protocols             | Error codes, instruction names, account metadata             |
| **Protocol GitHub repos**        | Direct              | Varies                    | Most major protocols publish IDLs in their repos             |
| **getIDL.xyz / Solvitor**        | Reverse-engineering | Anchor programs only      | Recovers IDLs from bytecode, imperfect results               |
| **`native-to-anchor`**           | Generator           | Native programs           | Generates Anchor-compatible IDL wrappers                     |
| **Shank + Codama**               | Generator           | Native programs           | Generate IDLs from annotated Rust code                       |
| **`solana-include-idl`**         | ELF embedding       | Emerging                  | IDL stored in program binary, CLI to extract                 |

---

## Risk Assessment for Solarix

### High Confidence (will work)

- Fetching IDLs for top DeFi protocols (Jupiter, Raydium, Orca, Meteora, Marinade, Drift)
- Bundling curated IDLs for SPL Token, Token-2022, System Program
- Manual IDL upload fallback

### Medium Confidence (needs validation)

- `solana_toolbox_idl` crate supporting PMP (may need patches or custom implementation)
- Distinguishing between legacy (pre-0.30) and new (0.30+) IDL formats on-chain
- Handling programs with maliciously claimed IDL accounts

### Low Confidence (uncertain)

- Automatic IDL discovery for arbitrary unknown programs
- Coverage beyond top ~200 programs without curated registry
- Real-time detection of whether a program uses legacy Anchor IDL vs PMP

### Recommendation

**Do not make "program ID only" the SOLE entry point.** Make it the PRIMARY and default flow, but design the architecture to gracefully handle missing IDLs with clear user messaging and a manual upload path. Ship a curated IDL bundle covering the top ~100 protocols (easily sourced from AllenHark library) to dramatically improve the out-of-box experience.

---

## Sources

### Primary Documentation

- [Solana IDLs Guide](https://solana.com/developers/guides/advanced/idls)
- [Anchor IDL File Docs](https://www.anchor-lang.com/docs/basics/idl)
- [Anchor v1.0.0 Release Notes](https://www.anchor-lang.com/docs/updates/release-notes/1-0-0)
- [Anchor v0.30.0 Release Notes](https://www.anchor-lang.com/docs/updates/release-notes/0-30-0)
- [Anchor v0.32.0 Release Notes](https://www.anchor-lang.com/docs/updates/release-notes/0-32-0)
- [Anchor CLI Reference](https://www.anchor-lang.com/docs/references/cli)

### Crate References

- [solana_toolbox_idl on crates.io](https://crates.io/crates/solana_toolbox_idl)
- [solana_toolbox_idl docs.rs](https://docs.rs/solana_toolbox_idl)
- [crypto-vincent/solana-toolbox GitHub](https://github.com/crypto-vincent/solana-toolbox)
- [solana-include-idl on crates.io](https://crates.io/crates/solana-include-idl)

### IDL Sources & Registries

- [AllenHark Solana IDL Library (70+ IDLs)](https://allenhark.com/solana-idl-library)
- [DeployDAO Solana Program Index](https://github.com/DeployDAO/solana-program-index)
- [Helius Orb Explorer](https://www.helius.dev/docs/orb/explore-programs)
- [tenequm/solana-idls (41+ protocols)](https://github.com/tenequm/solana-idls)
- [Program Metadata Program (PMP) GitHub](https://github.com/solana-program/program-metadata)

### Protocol-Specific IDL Sources

- [Jupiter CPI IDL](https://github.com/jup-ag/jupiter-cpi/blob/main/idl.json)
- [Raydium IDL Repo](https://github.com/raydium-io/raydium-idl)
- [Orca Whirlpool IDL Docs](https://dev.orca.so/More%20Resources/IDL/)
- [Marinade Anchor IDL Docs](https://docs.marinade.finance/developers/anchor-idl)
- [Tensor Foundation GitHub](https://github.com/tensor-foundation)

### IDL Recovery & Reverse-Engineering

- [getIDL.xyz / Sec3 IDL Guesser](https://www.sec3.dev/blog/idl-guesser-recovering-instruction-layouts-from-closed-source-solana-programs)
- [Solvitor (AI-powered IDL extraction)](https://solvitor.xyz/)
- [Hidden IDL Instructions and Abuse (Accretion)](https://accretionxyz.substack.com/p/hidden-idl-instructions-and-how-to)

### Standards & Community

- [sRFC 00008: IDL Standard](https://forum.solana.com/t/srfc-00008-idl-standard/66)
- [Improving IDLs Discoverability (dev.to)](https://dev.to/aseneca/improving-idls-discoverability-accelerating-solana-development-integration-and-composability-4oae)
- [The Transparency Problem in Solana (getIDL)](https://itnext.io/the-transparency-problem-in-solana-and-how-getidl-is-helping-to-solve-it-5ed472754221)
- [Anchor IDL and Managing Older Versions (blog.chalda.cz)](https://blog.chalda.cz/posts/anchor-idl/)
