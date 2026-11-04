//! solinv harness for `psol-vault` — deposit and withdraw.
//!
//! Loads the real `psol_compliance.so` and `psol_vault.so` from
//! `target/deploy/` (`./scripts/build-sbf.sh`). Two verified depositors,
//! A and B, each hold a position in one vault. The specs below send A's
//! deposit and withdraw, and solinv swaps in B's record, B's position, or
//! a token account the vault does not hold; any swap that succeeds and
//! changes state is a finding.

use crucible_fuzzer::*;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use std::str::FromStr;
use std::sync::Arc;

use solinv_core::invariants::account_swap::SwapSet;
use solinv_fuzz::{HasContext, HasInstructionSet, InstructionSpec};

const COMPLIANCE_SO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/psol_compliance.so");
const VAULT_SO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/psol_vault.so");
const NOOP_SO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/test_noop.so");

const COMPLIANCE_ID: Pubkey = Pubkey::new_from_array([7u8; 32]);
const VAULT_ID: Pubkey = Pubkey::new_from_array([9u8; 32]);
/// Stands in for a compliance program the vault was not set up with.
const ROGUE_ID: Pubkey = Pubkey::new_from_array([5u8; 32]);
const SYSTEM_ID: Pubkey = Pubkey::new_from_array([0u8; 32]);
const TOKEN_2022_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

// psol-compliance instruction tags and status codes.
const IX_INITIALIZE_REGISTRY: u8 = 0;
const IX_REGISTER_ACCOUNT: u8 = 1;
const IX_UPDATE_STATUS: u8 = 2;
const STATUS_VERIFIED: u8 = 2;
const CHANGE_STATUS: u8 = 0b0000_0001;

// psol-vault instruction tags.
const IX_INIT_VAULT: u8 = 0;
const IX_DEPOSIT: u8 = 1;
const IX_WITHDRAW: u8 = 2;

const START_BALANCE: u64 = 1_000_000;
const START_DEPOSIT: u64 = 100_000;

fn t22() -> Pubkey {
    Pubkey::from_str(TOKEN_2022_ID).unwrap()
}
/// Where the forged record lives: any key that is not a program id.
const FORGED_RECORD: Pubkey = Pubkey::new_from_array([0x42; 32]);

fn registry() -> Pubkey {
    Pubkey::find_program_address(&[b"registry"], &COMPLIANCE_ID).0
}
fn record(participant: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"account_record", participant.as_ref()], &COMPLIANCE_ID).0
}
fn vault_pda(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"vault", mint.as_ref()], &VAULT_ID).0
}
fn position(vault: &Pubkey, depositor: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"position", vault.as_ref(), depositor.as_ref()], &VAULT_ID).0
}

/// A Token-2022 mint with no extensions (82 bytes).
fn mint_data(decimals: u8) -> Vec<u8> {
    let mut d = vec![0u8; 82];
    d[36..44].copy_from_slice(&(START_BALANCE * 4).to_le_bytes()); // supply
    d[44] = decimals;
    d[45] = 1; // is_initialized
    d
}

/// A Token-2022 token account with no extensions (165 bytes).
fn token_account_data(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut d = vec![0u8; 165];
    d[0..32].copy_from_slice(mint.as_ref());
    d[32..64].copy_from_slice(owner.as_ref());
    d[64..72].copy_from_slice(&amount.to_le_bytes());
    d[108] = 1; // state = Initialized
    d
}

fn amount_data(tag: u8, amount: u64) -> Vec<u8> {
    let mut d = vec![tag];
    d.extend_from_slice(&amount.to_le_bytes());
    d
}

#[derive(Clone)]
struct VaultFixture {
    pub ctx: TestContext,
    payer: Arc<Keypair>,
    a: Arc<Keypair>,
    b: Arc<Keypair>,
    mint: Pubkey,
    vault: Pubkey,
    vault_ata: Pubkey,
    a_ata: Pubkey,
    a_ata2: Pubkey,
    b_ata: Pubkey,
}

impl VaultFixture {
    fn send(&mut self, ix: Instruction, signers: &[&Keypair], what: &str) {
        let payer = Arc::clone(&self.payer);
        let mut all: Vec<&Keypair> = vec![&*payer];
        all.extend(signers.iter().copied().filter(|k| k.pubkey() != payer.pubkey()));
        let outcome = self
            .ctx
            .raw_call(ix)
            .fee_payer(&*payer)
            .signers(&all)
            .send()
            .unwrap_or_else(|e| panic!("{what}: send: {e:?}"));
        // send() is Ok when the program fails; check the outcome.
        assert!(outcome.is_success(), "{what} failed: {outcome:?}");
    }

    fn put(&mut self, key: Pubkey, owner: Pubkey, data: Vec<u8>) {
        self.ctx
            .write_account(&key, Account { lamports: 10_000_000, data, owner, executable: false, rent_epoch: 0 })
            .expect("write account");
    }

    fn deposit_ix(&self, who: &Pubkey, ata: &Pubkey, amount: u64) -> Instruction {
        Instruction {
            program_id: VAULT_ID,
            accounts: self.deposit_metas(who, ata),
            data: amount_data(IX_DEPOSIT, amount),
        }
    }

    fn deposit_metas(&self, who: &Pubkey, ata: &Pubkey) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(*who, true),
            AccountMeta::new(self.payer.pubkey(), true),
            AccountMeta::new_readonly(record(who), false),
            AccountMeta::new_readonly(COMPLIANCE_ID, false),
            AccountMeta::new(self.vault, false),
            AccountMeta::new(position(&self.vault, who), false),
            AccountMeta::new_readonly(self.mint, false),
            AccountMeta::new(*ata, false),
            AccountMeta::new(self.vault_ata, false),
            AccountMeta::new_readonly(t22(), false),
            AccountMeta::new_readonly(SYSTEM_ID, false),
        ]
    }

    fn withdraw_metas(&self, who: &Pubkey, ata: &Pubkey) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(*who, true),
            AccountMeta::new_readonly(record(who), false),
            AccountMeta::new_readonly(COMPLIANCE_ID, false),
            AccountMeta::new(self.vault, false),
            AccountMeta::new(position(&self.vault, who), false),
            AccountMeta::new_readonly(self.mint, false),
            AccountMeta::new(self.vault_ata, false),
            AccountMeta::new(*ata, false),
            AccountMeta::new_readonly(t22(), false),
        ]
    }

    fn send_ok(&mut self, ix: Instruction, signer: Arc<Keypair>) -> bool {
        let payer = Arc::clone(&self.payer);
        self.ctx
            .raw_call(ix)
            .fee_payer(&*payer)
            .signers(&[&*payer, &*signer])
            .send()
            .map(|o| o.is_success())
            .unwrap_or(false)
    }
}

#[fuzz_fixture]
impl VaultFixture {
    pub fn setup() -> Self {
        let mut ctx = TestContext::new();
        ctx.add_program(&COMPLIANCE_ID, COMPLIANCE_SO)
            .expect("load psol_compliance.so — run ./scripts/build-sbf.sh");
        ctx.add_program(&VAULT_ID, VAULT_SO)
            .expect("load psol_vault.so — run ./scripts/build-sbf.sh");
        ctx.add_program(&ROGUE_ID, NOOP_SO)
            .expect("load test_noop.so — run ./scripts/build-sbf.sh");

        let payer = Arc::new(Keypair::new());
        let a = Arc::new(Keypair::new());
        let b = Arc::new(Keypair::new());
        for kp in [&payer, &a, &b] {
            ctx.create_account()
                .pubkey(kp.pubkey())
                .lamports(10_000_000_000)
                .owner(SYSTEM_ID)
                .create()
                .unwrap();
        }

        let mint = Keypair::new().pubkey();
        let vault = vault_pda(&mint);
        let mut f = Self {
            ctx,
            payer: Arc::clone(&payer),
            a: Arc::clone(&a),
            b: Arc::clone(&b),
            mint,
            vault,
            vault_ata: Keypair::new().pubkey(),
            a_ata: Keypair::new().pubkey(),
            a_ata2: Keypair::new().pubkey(),
            b_ata: Keypair::new().pubkey(),
        };

        // Compliance: registry, then A and B registered and verified.
        let p = payer.pubkey();
        f.send(
            Instruction {
                program_id: COMPLIANCE_ID,
                accounts: vec![
                    AccountMeta::new(p, true),
                    AccountMeta::new(registry(), false),
                    AccountMeta::new_readonly(SYSTEM_ID, false),
                ],
                data: vec![IX_INITIALIZE_REGISTRY],
            },
            &[],
            "initialize_registry",
        );
        for who in [a.pubkey(), b.pubkey()] {
            let mut data = vec![IX_REGISTER_ACCOUNT];
            data.extend_from_slice(who.as_ref());
            data.extend_from_slice(b"JP");
            f.send(
                Instruction {
                    program_id: COMPLIANCE_ID,
                    accounts: vec![
                        AccountMeta::new_readonly(p, true),
                        AccountMeta::new(p, true),
                        AccountMeta::new_readonly(registry(), false),
                        AccountMeta::new(record(&who), false),
                        AccountMeta::new_readonly(SYSTEM_ID, false),
                    ],
                    data,
                },
                &[],
                "register_account",
            );
            f.send(
                Instruction {
                    program_id: COMPLIANCE_ID,
                    accounts: vec![
                        AccountMeta::new_readonly(p, true),
                        AccountMeta::new_readonly(registry(), false),
                        AccountMeta::new(record(&who), false),
                    ],
                    data: vec![IX_UPDATE_STATUS, STATUS_VERIFIED, 0, CHANGE_STATUS],
                },
                &[],
                "update_status",
            );
        }

        // Mint and token accounts, written directly.
        f.put(mint, t22(), mint_data(6));
        f.send(
            Instruction {
                program_id: VAULT_ID,
                accounts: vec![
                    AccountMeta::new(p, true),
                    AccountMeta::new(vault, false),
                    AccountMeta::new_readonly(mint, false),
                    AccountMeta::new_readonly(SYSTEM_ID, false),
                    AccountMeta::new_readonly(COMPLIANCE_ID, false),
                ],
                data: vec![IX_INIT_VAULT],
            },
            &[],
            "init_vault",
        );
        let (va, aa, aa2, ba) = (f.vault_ata, f.a_ata, f.a_ata2, f.b_ata);
        f.put(va, t22(), token_account_data(&mint, &vault, 0));
        f.put(aa, t22(), token_account_data(&mint, &a.pubkey(), START_BALANCE));
        f.put(aa2, t22(), token_account_data(&mint, &a.pubkey(), 0));
        f.put(ba, t22(), token_account_data(&mint, &b.pubkey(), START_BALANCE));

        // Both depositors hold a position.
        let ix = f.deposit_ix(&a.pubkey(), &aa, START_DEPOSIT);
        f.send(ix, &[&*a], "deposit A");
        let ix = f.deposit_ix(&b.pubkey(), &ba, START_DEPOSIT);
        f.send(ix, &[&*b], "deposit B");
        // A copy of A's verified record, owned by the rogue program: the
        // fake-registry attack swaps it in together with that program.
        let rec = f.ctx.get_account(&record(&a.pubkey())).unwrap().data.clone();
        f.put(FORGED_RECORD, ROGUE_ID, rec);
        f
    }

    pub fn action_deposit(&mut self, #[range(1..10_000)] amount: u64) -> bool {
        let ix = self.deposit_ix(&self.a.pubkey(), &self.a_ata, amount);
        self.send_ok(ix, Arc::clone(&self.a))
    }

    pub fn action_withdraw(&mut self, #[range(1..10_000)] amount: u64) -> bool {
        let ix = Instruction {
            program_id: VAULT_ID,
            accounts: self.withdraw_metas(&self.a.pubkey(), &self.a_ata),
            data: amount_data(IX_WITHDRAW, amount),
        };
        self.send_ok(ix, Arc::clone(&self.a))
    }
}

impl HasContext for VaultFixture {
    fn ctx(&self) -> &TestContext {
        &self.ctx
    }
    fn ctx_mut(&mut self) -> &mut TestContext {
        &mut self.ctx
    }
    fn program_ids(&self) -> Vec<Pubkey> {
        vec![VAULT_ID]
    }
    fn fee_payer(&self) -> Arc<Keypair> {
        Arc::clone(&self.payer)
    }
}

impl HasInstructionSet for VaultFixture {
    fn instructions(&self) -> Vec<InstructionSpec> {
        let a = self.a.pubkey();
        let b = self.b.pubkey();
        let t = Some(t22());
        let v = Some(VAULT_ID);
        let c = Some(COMPLIANCE_ID);
        let deposit = InstructionSpec {
            program_id: VAULT_ID,
            name: "deposit".into(),
            accounts: self.deposit_metas(&a, &self.a_ata),
            // [1] is the fee payer, which always signs; only [0] can be
            // left unsigned.
            signer_indices: vec![0],
            optional_signer_indices: vec![],
            expected_owners: vec![None, None, c, None, v, v, t, t, t, None, None],
            expected_discriminators: vec![None; 11],
            expected_pda_seeds: vec![None; 11],
            creates_indices: vec![],
            swap_alternates: vec![
                vec![],
                vec![],
                vec![record(&b)],                  // B's record for A's deposit
                vec![],
                vec![],
                vec![position(&self.vault, &b)],  // credit B's position
                vec![],
                vec![],
                vec![self.a_ata2],                 // "vault" account A holds
                vec![],
                vec![],
            ],
            data_sample: amount_data(IX_DEPOSIT, 10),
            signers: vec![Arc::clone(&self.a)],
            state_invariants: vec![],
            cu_budget: None,
            cpi_reentrancy: None,
            realloc_check: None,
            bump_seed_check: None,
            guards: vec![],
        };
        let withdraw = InstructionSpec {
            program_id: VAULT_ID,
            name: "withdraw".into(),
            accounts: self.withdraw_metas(&a, &self.a_ata),
            signer_indices: vec![0],
            optional_signer_indices: vec![],
            expected_owners: vec![None, c, None, v, v, t, t, t, None],
            expected_discriminators: vec![None; 9],
            expected_pda_seeds: vec![None; 9],
            creates_indices: vec![],
            swap_alternates: vec![
                vec![],
                vec![record(&b)],                  // B's record
                vec![ROGUE_ID],                    // another compliance program
                vec![],
                vec![position(&self.vault, &b)],  // debit B's position
                vec![],
                vec![self.a_ata2],                 // "vault" account A holds
                vec![self.b_ata],                  // pay out to B
                vec![],
            ],
            data_sample: amount_data(IX_WITHDRAW, 10),
            signers: vec![Arc::clone(&self.a)],
            state_invariants: vec![],
            cu_budget: None,
            cpi_reentrancy: None,
            realloc_check: None,
            bump_seed_check: None,
            guards: vec![],
        };
        vec![deposit, withdraw]
    }
}

#[invariant_test]
fn invariant_vault(fixture: &mut VaultFixture) {
    solinv_core::invariants::signer_skip::check(fixture);
    solinv_core::invariants::owner_skip::check(fixture);
    solinv_core::invariants::account_swap::check(fixture);
    // Another compliance program and a record it owns, swapped together:
    // each alone fails, so one-slot swaps cannot see a missing pin.
    solinv_core::invariants::account_swap::check_sets(
        fixture,
        &[
            SwapSet { ix: "deposit".into(), swaps: vec![(2, FORGED_RECORD), (3, ROGUE_ID)] },
            SwapSet { ix: "withdraw".into(), swaps: vec![(1, FORGED_RECORD), (2, ROGUE_ID)] },
        ],
    );
}
