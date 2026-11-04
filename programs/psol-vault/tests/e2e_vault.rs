//! End-to-end test for the collateral vault primitive.
//!
//! Walks through:
//!   1. Compliance bootstrap and a verified depositor.
//!   2. Plain Token-2022 mint (Phase 2 = Model C, no TransferHook).
//!   3. `init_vault` — vault PDA + canonical vault ATA owned by the PDA.
//!   4. Two `deposit`s (verifies idempotent position create + aggregation).
//!   5. Partial then full `withdraw` (verifies bookkeeping + Token-2022
//!      transfer signed by the vault PDA).
//!   6. Negative paths: suspended depositor → withdraw rejects;
//!      over-withdraw rejects.
//!
//! Prerequisite: `target/deploy/psol_compliance.so` and
//! `target/deploy/psol_vault.so` must exist (`cargo build-sbf`).

use {
    bytemuck::from_bytes,
    litesvm::LiteSVM,
    solana_address::Address,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_message::Message,
    solana_program_pack::Pack,
    solana_signer::Signer,
    solana_transaction::Transaction,
    spl_token_2022::state::{Account as TokenAccount, Mint},
    psol_types::{asset_class, compliance_status, seeds, Position, Vault},
    psol_vault::{err as vault_err, ix as vault_ix},
    std::path::PathBuf,
};

// ─── Harness ─────────────────────────────────────────────────────────────

fn compliance_program_id() -> Address {
    Address::from([7u8; 32])
}
fn vault_program_id() -> Address {
    Address::from([9u8; 32])
}
fn token_2022_id() -> Address {
    spl_token_2022::ID.to_bytes().into()
}

const ATA_PROGRAM_ID: Address = Address::new_from_array(pinocchio_pubkey::from_str(
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
));

fn so_path(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/deploy/");
    p.push(name);
    p
}

fn setup() -> Option<(LiteSVM, Keypair)> {
    for (label, path) in [
        ("psol_compliance.so", so_path("psol_compliance.so")),
        ("psol_vault.so", so_path("psol_vault.so")),
    ] {
        if !path.exists() {
            eprintln!(
                "{label} not built. Run `cargo build-sbf` on each program first."
            );
            if std::env::var_os("PSOL_SKIP_E2E").is_none() {
                panic!("SBF build missing: run ./scripts/build-sbf.sh (or set PSOL_SKIP_E2E=1 to skip)");
            }
            return None;
        }
    }
    let mut svm = LiteSVM::new();
    svm.add_program(
        compliance_program_id(),
        &std::fs::read(so_path("psol_compliance.so")).unwrap(),
    )
    .unwrap();
    svm.add_program(
        vault_program_id(),
        &std::fs::read(so_path("psol_vault.so")).unwrap(),
    )
    .unwrap();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 100_000_000_000).unwrap();
    Some((svm, payer))
}

fn send(svm: &mut LiteSVM, ixs: &[Instruction], signers: &[&Keypair]) {
    let blockhash = svm.latest_blockhash();
    let payer = signers[0].pubkey();
    let msg = Message::new(ixs, Some(&payer));
    let tx = Transaction::new(signers, msg, blockhash);
    svm.send_transaction(tx).expect("tx failed");
}
fn try_send(
    svm: &mut LiteSVM,
    ixs: &[Instruction],
    signers: &[&Keypair],
) -> Result<(), litesvm::types::FailedTransactionMetadata> {
    let blockhash = svm.latest_blockhash();
    let payer = signers[0].pubkey();
    let msg = Message::new(ixs, Some(&payer));
    let tx = Transaction::new(signers, msg, blockhash);
    svm.send_transaction(tx).map(|_| ())
}

// ─── Derivations ─────────────────────────────────────────────────────────

fn derive_registry() -> Address {
    Address::find_program_address(&[seeds::REGISTRY], &compliance_program_id()).0
}
fn derive_record(participant: &Address) -> Address {
    Address::find_program_address(
        &[seeds::ACCOUNT_RECORD, participant.as_ref()],
        &compliance_program_id(),
    )
    .0
}
fn derive_vault(mint: &Address) -> Address {
    Address::find_program_address(&[seeds::VAULT, mint.as_ref()], &vault_program_id()).0
}
fn derive_position(vault: &Address, depositor: &Address) -> Address {
    Address::find_program_address(
        &[seeds::POSITION, vault.as_ref(), depositor.as_ref()],
        &vault_program_id(),
    )
    .0
}
fn derive_canonical_ata(owner: &Address, mint: &Address) -> Address {
    Address::find_program_address(
        &[owner.as_ref(), token_2022_id().as_ref(), mint.as_ref()],
        &ATA_PROGRAM_ID,
    )
    .0
}

// ─── Compliance instruction builders (mirrors smoke.rs) ─────────────────-

fn ix_initialize_registry(payer: &Address) -> Instruction {
    Instruction {
        program_id: compliance_program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(derive_registry(), false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        ],
        data: vec![psol_compliance::ix::INITIALIZE_REGISTRY],
    }
}
fn ix_register_account(payer: &Address, participant: &Address) -> Instruction {
    let mut data = Vec::with_capacity(1 + 32 + 2);
    data.push(psol_compliance::ix::REGISTER_ACCOUNT);
    data.extend_from_slice(participant.as_ref());
    data.extend_from_slice(b"JP");
    Instruction {
        program_id: compliance_program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*payer, true),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(derive_registry(), false),
            AccountMeta::new(derive_record(participant), false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        ],
        data,
    }
}
fn ix_update_status(payer: &Address, participant: &Address, status: u8) -> Instruction {
    Instruction {
        program_id: compliance_program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*payer, true),
            AccountMeta::new_readonly(derive_registry(), false),
            AccountMeta::new(derive_record(participant), false),
        ],
        data: vec![
            psol_compliance::ix::UPDATE_STATUS,
            status,
            0,
            psol_compliance::change_mask::STATUS,
        ],
    }
}

// ─── Token-2022 helpers (mirror e2e_dvp.rs's no-hook flavor) ────────────-

fn create_plain_mint(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint_kp: &Keypair,
    mint_authority: &Address,
    decimals: u8,
) {
    let space = Mint::LEN;
    let lamports = svm.minimum_balance_for_rent_exemption(space);
    let create = solana_system_interface::instruction::create_account(
        &payer.pubkey(),
        &mint_kp.pubkey(),
        lamports,
        space as u64,
        &spl_token_2022::ID,
    );
    let init = spl_token_2022::instruction::initialize_mint2(
        &spl_token_2022::ID,
        &mint_kp.pubkey(),
        mint_authority,
        None,
        decimals,
    )
    .unwrap();
    send(svm, &[create, init], &[payer, mint_kp]);
}

fn create_user_ata(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Address,
    owner: &Address,
) -> Address {
    let ata = derive_canonical_ata(owner, mint);
    let ix = Instruction {
        program_id: ATA_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(ata, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(token_2022_id(), false),
        ],
        data: vec![1u8], // CreateIdempotent
    };
    send(svm, &[ix], &[payer]);
    ata
}

fn mint_to(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Address,
    authority: &Keypair,
    dest: &Address,
    amount: u64,
) {
    let ix = spl_token_2022::instruction::mint_to(
        &spl_token_2022::ID,
        mint,
        dest,
        &authority.pubkey(),
        &[],
        amount,
    )
    .unwrap();
    send(svm, &[ix], &[payer, authority]);
}

fn balance(svm: &LiteSVM, token_account: &Address) -> u64 {
    let acc = svm.get_account(token_account).unwrap();
    TokenAccount::unpack(&acc.data[..TokenAccount::LEN])
        .unwrap()
        .amount
}

// ─── Vault instruction builders ──────────────────────────────────────────

fn ix_init_vault(admin: &Address, mint: &Address) -> Instruction {
    Instruction {
        program_id: vault_program_id(),
        accounts: vec![
            AccountMeta::new(*admin, true),
            AccountMeta::new(derive_vault(mint), false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(compliance_program_id(), false),
        ],
        data: vec![vault_ix::INIT_VAULT],
    }
}

#[allow(clippy::too_many_arguments)]
fn ix_deposit(
    depositor: &Address,
    payer: &Address,
    mint: &Address,
    depositor_ata: &Address,
    vault_ata: &Address,
    amount: u64,
) -> Instruction {
    let vault = derive_vault(mint);
    let mut data = Vec::with_capacity(1 + 8);
    data.push(vault_ix::DEPOSIT);
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: vault_program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*depositor, true),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(derive_record(depositor), false),
            AccountMeta::new_readonly(compliance_program_id(), false),
            AccountMeta::new(vault, false),
            AccountMeta::new(derive_position(&vault, depositor), false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*depositor_ata, false),
            AccountMeta::new(*vault_ata, false),
            AccountMeta::new_readonly(token_2022_id(), false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        ],
        data,
    }
}

fn ix_withdraw(
    depositor: &Address,
    mint: &Address,
    vault_ata: &Address,
    depositor_ata: &Address,
    amount: u64,
) -> Instruction {
    let vault = derive_vault(mint);
    let mut data = Vec::with_capacity(1 + 8);
    data.push(vault_ix::WITHDRAW);
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: vault_program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*depositor, true),
            AccountMeta::new_readonly(derive_record(depositor), false),
            AccountMeta::new_readonly(compliance_program_id(), false),
            AccountMeta::new(vault, false),
            AccountMeta::new(derive_position(&vault, depositor), false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*vault_ata, false),
            AccountMeta::new(*depositor_ata, false),
            AccountMeta::new_readonly(token_2022_id(), false),
        ],
        data,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[test]
fn full_lifecycle_deposit_partial_withdraw_full_withdraw() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };

    // Compliance bootstrap.
    send(&mut svm, &[ix_initialize_registry(&payer.pubkey())], &[&payer]);

    // Token-2022 mint (plain).
    let mint_kp = Keypair::new();
    create_plain_mint(&mut svm, &payer, &mint_kp, &payer.pubkey(), 6);
    let mint = mint_kp.pubkey();

    // Depositor + verify.
    let depositor = Keypair::new();
    svm.airdrop(&depositor.pubkey(), 1_000_000_000).unwrap();
    send(
        &mut svm,
        &[ix_register_account(&payer.pubkey(), &depositor.pubkey())],
        &[&payer],
    );
    send(
        &mut svm,
        &[ix_update_status(
            &payer.pubkey(),
            &depositor.pubkey(),
            compliance_status::VERIFIED,
        )],
        &[&payer],
    );

    // Depositor ATA + mint tokens to it.
    let depositor_ata = create_user_ata(&mut svm, &payer, &mint, &depositor.pubkey());
    mint_to(&mut svm, &payer, &mint, &payer, &depositor_ata, 1_000_000);

    // init_vault — admin = payer.
    send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint)], &[&payer]);
    let vault = derive_vault(&mint);
    // Vault account exists and decodes.
    let v_acc = svm.get_account(&vault).expect("vault PDA exists");
    let v: &Vault = from_bytes(&v_acc.data[..Vault::LEN]);
    assert_eq!(v.mint, mint.to_bytes());
    assert_eq!(v.admin, payer.pubkey().to_bytes());
    assert_eq!(v.total_deposited, 0);
    assert_eq!(v.position_count, 0);

    // Create the vault's canonical ATA — same path the deposit relies on.
    let vault_ata = create_user_ata(&mut svm, &payer, &mint, &vault);

    // First deposit: 500_000. Should idempotently create the position.
    send(
        &mut svm,
        &[ix_deposit(
            &depositor.pubkey(),
            &payer.pubkey(),
            &mint,
            &depositor_ata,
            &vault_ata,
            500_000,
        )],
        &[&payer, &depositor],
    );
    assert_eq!(balance(&svm, &vault_ata), 500_000);
    let v_acc = svm.get_account(&vault).unwrap();
    let v: &Vault = from_bytes(&v_acc.data[..Vault::LEN]);
    assert_eq!(v.total_deposited, 500_000);
    assert_eq!(v.position_count, 1);
    let position = derive_position(&vault, &depositor.pubkey());
    let p_acc = svm.get_account(&position).expect("position PDA exists");
    let p: &Position = from_bytes(&p_acc.data[..Position::LEN]);
    assert_eq!(p.amount_deposited, 500_000);
    assert_eq!(p.locked_amount, 0);
    assert_eq!(p.available(), 500_000);

    // Second deposit: 300_000. Position aggregates.
    send(
        &mut svm,
        &[ix_deposit(
            &depositor.pubkey(),
            &payer.pubkey(),
            &mint,
            &depositor_ata,
            &vault_ata,
            300_000,
        )],
        &[&payer, &depositor],
    );
    let v_acc = svm.get_account(&vault).unwrap();
    let v: &Vault = from_bytes(&v_acc.data[..Vault::LEN]);
    assert_eq!(v.total_deposited, 800_000);
    assert_eq!(v.position_count, 1, "second deposit must not bump count");
    let p_acc = svm.get_account(&position).unwrap();
    let p: &Position = from_bytes(&p_acc.data[..Position::LEN]);
    assert_eq!(p.amount_deposited, 800_000);

    // Partial withdraw: 200_000.
    send(
        &mut svm,
        &[ix_withdraw(
            &depositor.pubkey(),
            &mint,
            &vault_ata,
            &depositor_ata,
            200_000,
        )],
        &[&payer, &depositor],
    );
    assert_eq!(balance(&svm, &vault_ata), 600_000);
    let p_acc = svm.get_account(&position).unwrap();
    let p: &Position = from_bytes(&p_acc.data[..Position::LEN]);
    assert_eq!(p.amount_deposited, 600_000);

    // Full withdraw of remainder: 600_000.
    send(
        &mut svm,
        &[ix_withdraw(
            &depositor.pubkey(),
            &mint,
            &vault_ata,
            &depositor_ata,
            600_000,
        )],
        &[&payer, &depositor],
    );
    assert_eq!(balance(&svm, &vault_ata), 0);
    let p_acc = svm.get_account(&position).unwrap();
    let p: &Position = from_bytes(&p_acc.data[..Position::LEN]);
    assert_eq!(p.amount_deposited, 0);
    assert_eq!(p.available(), 0);
    let v_acc = svm.get_account(&vault).unwrap();
    let v: &Vault = from_bytes(&v_acc.data[..Vault::LEN]);
    assert_eq!(v.total_deposited, 0);
    assert_eq!(v.position_count, 1, "position not auto-closed at zero (Phase 2)");
}

#[test]
fn withdraw_rejects_when_depositor_suspended() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    send(&mut svm, &[ix_initialize_registry(&payer.pubkey())], &[&payer]);

    let mint_kp = Keypair::new();
    create_plain_mint(&mut svm, &payer, &mint_kp, &payer.pubkey(), 6);
    let mint = mint_kp.pubkey();

    let depositor = Keypair::new();
    svm.airdrop(&depositor.pubkey(), 1_000_000_000).unwrap();
    send(
        &mut svm,
        &[ix_register_account(&payer.pubkey(), &depositor.pubkey())],
        &[&payer],
    );
    send(
        &mut svm,
        &[ix_update_status(
            &payer.pubkey(),
            &depositor.pubkey(),
            compliance_status::VERIFIED,
        )],
        &[&payer],
    );

    let depositor_ata = create_user_ata(&mut svm, &payer, &mint, &depositor.pubkey());
    mint_to(&mut svm, &payer, &mint, &payer, &depositor_ata, 1_000_000);

    send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint)], &[&payer]);
    let vault = derive_vault(&mint);
    let vault_ata = create_user_ata(&mut svm, &payer, &mint, &vault);

    send(
        &mut svm,
        &[ix_deposit(
            &depositor.pubkey(),
            &payer.pubkey(),
            &mint,
            &depositor_ata,
            &vault_ata,
            500_000,
        )],
        &[&payer, &depositor],
    );

    // Suspend the depositor *after* they deposited. Withdraw must
    // reject — even though the tokens are technically theirs.
    send(
        &mut svm,
        &[ix_update_status(
            &payer.pubkey(),
            &depositor.pubkey(),
            compliance_status::SUSPENDED,
        )],
        &[&payer],
    );

    let res = try_send(
        &mut svm,
        &[ix_withdraw(
            &depositor.pubkey(),
            &mint,
            &vault_ata,
            &depositor_ata,
            500_000,
        )],
        &[&payer, &depositor],
    );
    assert!(res.is_err(), "withdraw must reject when depositor suspended");
    assert_eq!(balance(&svm, &vault_ata), 500_000, "tokens stuck in vault");
}

#[test]
fn over_withdraw_rejects() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    send(&mut svm, &[ix_initialize_registry(&payer.pubkey())], &[&payer]);

    let mint_kp = Keypair::new();
    create_plain_mint(&mut svm, &payer, &mint_kp, &payer.pubkey(), 6);
    let mint = mint_kp.pubkey();
    let depositor = Keypair::new();
    svm.airdrop(&depositor.pubkey(), 1_000_000_000).unwrap();
    send(
        &mut svm,
        &[ix_register_account(&payer.pubkey(), &depositor.pubkey())],
        &[&payer],
    );
    send(
        &mut svm,
        &[ix_update_status(
            &payer.pubkey(),
            &depositor.pubkey(),
            compliance_status::VERIFIED,
        )],
        &[&payer],
    );
    let depositor_ata = create_user_ata(&mut svm, &payer, &mint, &depositor.pubkey());
    mint_to(&mut svm, &payer, &mint, &payer, &depositor_ata, 1_000_000);
    send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint)], &[&payer]);
    let vault = derive_vault(&mint);
    let vault_ata = create_user_ata(&mut svm, &payer, &mint, &vault);
    send(
        &mut svm,
        &[ix_deposit(
            &depositor.pubkey(),
            &payer.pubkey(),
            &mint,
            &depositor_ata,
            &vault_ata,
            100_000,
        )],
        &[&payer, &depositor],
    );

    // Try to withdraw more than was deposited.
    let res = try_send(
        &mut svm,
        &[ix_withdraw(
            &depositor.pubkey(),
            &mint,
            &vault_ata,
            &depositor_ata,
            500_000,
        )],
        &[&payer, &depositor],
    );
    assert!(res.is_err(), "over-withdraw must reject");
    assert_eq!(balance(&svm, &vault_ata), 100_000);
}

// Touch the error namespace import for the compiler to confirm we use it.
#[allow(dead_code)]
fn _use_vault_err() {
    let _ = vault_err::INSUFFICIENT_AVAILABLE;
    let _ = vault_err::COMPLIANCE_SUSPENDED;
}

// ─── Phase 4 v0: asset_class round-trip ─────────────────────────────────
//
// The vault's on-chain init_vault now reads an optional trailing byte
// from instruction data and persists it as `Vault::asset_class`. This
// pins:
//   - Backwards compat: 1-byte ix data (the legacy shape) yields
//     `asset_class::UNKNOWN`, so any pre-Phase-4 caller keeps working
//     and that vault contributes zero margin credit.
//   - New shape: explicit 2-byte ix data persists the class verbatim.
// If anyone removes the `rest` threading or the `unwrap_or(UNKNOWN)`
// from process_init_vault, both halves of this test must fail.

fn ix_init_vault_with_class(admin: &Address, mint: &Address, class: u8) -> Instruction {
    Instruction {
        program_id: vault_program_id(),
        accounts: vec![
            AccountMeta::new(*admin, true),
            AccountMeta::new(derive_vault(mint), false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(compliance_program_id(), false),
        ],
        data: vec![vault_ix::INIT_VAULT, class],
    }
}

#[test]
fn init_vault_defaults_asset_class_to_unknown_when_data_is_legacy() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    let mint_kp = Keypair::new();
    create_plain_mint(&mut svm, &payer, &mint_kp, &payer.pubkey(), 6);
    let mint = mint_kp.pubkey();
    // Legacy shape: 1-byte ix data (just the discriminator).
    send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint)], &[&payer]);
    let v_acc = svm.get_account(&derive_vault(&mint)).unwrap();
    let v: Vault = *from_bytes(&v_acc.data[..Vault::LEN]);
    assert_eq!(
        v.asset_class,
        asset_class::UNKNOWN,
        "legacy 1-byte init_vault data must yield UNKNOWN (zero margin credit)"
    );
}

#[test]
fn init_vault_persists_explicit_asset_class() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    let mint_kp = Keypair::new();
    create_plain_mint(&mut svm, &payer, &mint_kp, &payer.pubkey(), 6);
    let mint = mint_kp.pubkey();
    send(
        &mut svm,
        &[ix_init_vault_with_class(&payer.pubkey(), &mint, asset_class::EQUITY)],
        &[&payer],
    );
    let v_acc = svm.get_account(&derive_vault(&mint)).unwrap();
    let v: Vault = *from_bytes(&v_acc.data[..Vault::LEN]);
    assert_eq!(
        v.asset_class,
        asset_class::EQUITY,
        "explicit asset_class byte must round-trip into Vault::asset_class"
    );
}

// ─── The vault trusts only the compliance program it was initialised with ─

fn rogue_compliance_program_id() -> Address {
    Address::from([8u8; 32])
}

/// Re-target a compliance instruction built for `compliance_program_id()`
/// at a second deployment, re-deriving the registry and record PDAs.
fn on_rogue(mut ix: Instruction, participant: Option<&Address>) -> Instruction {
    let rogue = rogue_compliance_program_id();
    let registry = derive_registry();
    let rogue_registry = Address::find_program_address(&[seeds::REGISTRY], &rogue).0;
    let record = participant.map(derive_record);
    let rogue_record = participant.map(|p| {
        Address::find_program_address(&[seeds::ACCOUNT_RECORD, p.as_ref()], &rogue).0
    });
    ix.program_id = rogue;
    for m in &mut ix.accounts {
        if m.pubkey == registry {
            m.pubkey = rogue_registry;
        } else if Some(m.pubkey) == record {
            m.pubkey = rogue_record.unwrap();
        }
    }
    ix
}

/// A suspended depositor deploys their own copy of the compliance program,
/// marks themselves VERIFIED there, and passes that program and record to
/// `withdraw`. The vault must refuse: it only accepts records owned by the
/// program it was initialised with.
#[test]
fn withdraw_rejects_records_from_another_compliance_program() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    svm.add_program(
        rogue_compliance_program_id(),
        &std::fs::read(so_path("psol_compliance.so")).unwrap(),
    )
    .unwrap();
    send(&mut svm, &[ix_initialize_registry(&payer.pubkey())], &[&payer]);

    let mint_kp = Keypair::new();
    create_plain_mint(&mut svm, &payer, &mint_kp, &payer.pubkey(), 6);
    let mint = mint_kp.pubkey();

    let depositor = Keypair::new();
    svm.airdrop(&depositor.pubkey(), 1_000_000_000).unwrap();
    let d = depositor.pubkey();
    send(&mut svm, &[ix_register_account(&payer.pubkey(), &d)], &[&payer]);
    send(
        &mut svm,
        &[ix_update_status(&payer.pubkey(), &d, compliance_status::VERIFIED)],
        &[&payer],
    );
    let depositor_ata = create_user_ata(&mut svm, &payer, &mint, &d);
    mint_to(&mut svm, &payer, &mint, &payer, &depositor_ata, 1_000_000);

    send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint)], &[&payer]);
    let vault = derive_vault(&mint);
    let vault_ata = create_user_ata(&mut svm, &payer, &mint, &vault);
    send(
        &mut svm,
        &[ix_deposit(&d, &payer.pubkey(), &mint, &depositor_ata, &vault_ata, 500_000)],
        &[&payer, &depositor],
    );
    send(
        &mut svm,
        &[ix_update_status(&payer.pubkey(), &d, compliance_status::SUSPENDED)],
        &[&payer],
    );

    // The depositor's own registry, where they are VERIFIED.
    send(
        &mut svm,
        &[
            on_rogue(ix_initialize_registry(&d), None),
            on_rogue(ix_register_account(&d, &d), Some(&d)),
            on_rogue(ix_update_status(&d, &d, compliance_status::VERIFIED), Some(&d)),
        ],
        &[&depositor],
    );

    let mut ix = ix_withdraw(&d, &mint, &vault_ata, &depositor_ata, 500_000);
    ix.accounts[1].pubkey =
        Address::find_program_address(&[seeds::ACCOUNT_RECORD, d.as_ref()], &rogue_compliance_program_id()).0;
    ix.accounts[2].pubkey = rogue_compliance_program_id();
    let res = try_send(&mut svm, &[ix], &[&payer, &depositor]);
    let err = format!("{:?}", res.expect_err("withdraw must reject a record from another program"));
    assert!(
        err.contains(&format!("Custom({})", 0x300C)),
        "expected COMPLIANCE_PROGRAM_MISMATCH, got {err}"
    );
    assert_eq!(balance(&svm, &vault_ata), 500_000, "tokens stay in the vault");

    let v_acc = svm.get_account(&vault).unwrap();
    let v: Vault = *from_bytes(&v_acc.data[..Vault::LEN]);
    assert_eq!(v.compliance_program, compliance_program_id().to_bytes());
}

/// A verified depositor "deposits" into a second token account of their
/// own instead of the vault's. The vault must refuse: otherwise the
/// position is credited with tokens the vault never received, and a
/// withdraw would pay them out of other depositors' tokens.
#[test]
fn deposit_rejects_a_destination_the_vault_does_not_hold() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    send(&mut svm, &[ix_initialize_registry(&payer.pubkey())], &[&payer]);
    let mint_kp = Keypair::new();
    create_plain_mint(&mut svm, &payer, &mint_kp, &payer.pubkey(), 6);
    let mint = mint_kp.pubkey();
    let depositor = Keypair::new();
    svm.airdrop(&depositor.pubkey(), 1_000_000_000).unwrap();
    let d = depositor.pubkey();
    send(&mut svm, &[ix_register_account(&payer.pubkey(), &d)], &[&payer]);
    send(&mut svm, &[ix_update_status(&payer.pubkey(), &d, compliance_status::VERIFIED)], &[&payer]);
    let depositor_ata = create_user_ata(&mut svm, &payer, &mint, &d);
    mint_to(&mut svm, &payer, &mint, &payer, &depositor_ata, 1_000_000);
    send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint)], &[&payer]);
    create_user_ata(&mut svm, &payer, &mint, &derive_vault(&mint));

    // A second account owned by the depositor, created with the system
    // program + initialize_account3 (not the ATA, which is the source).
    let other = Keypair::new();
    let lamports = svm.minimum_balance_for_rent_exemption(TokenAccount::LEN);
    let create = solana_system_interface::instruction::create_account(
        &payer.pubkey(), &other.pubkey(), lamports, TokenAccount::LEN as u64, &spl_token_2022::ID,
    );
    let init = spl_token_2022::instruction::initialize_account3(&spl_token_2022::ID, &other.pubkey(), &mint, &d).unwrap();
    send(&mut svm, &[create, init], &[&payer, &other]);

    let res = try_send(
        &mut svm,
        &[ix_deposit(&d, &payer.pubkey(), &mint, &depositor_ata, &other.pubkey(), 500_000)],
        &[&payer, &depositor],
    );
    let err = format!("{:?}", res.expect_err("deposit into the depositor's own account must reject"));
    assert!(err.contains(&format!("Custom({})", 0x300E)), "expected TOKEN_ACCOUNT_MISMATCH, got {err}");
    assert_eq!(balance(&svm, &depositor_ata), 1_000_000);
}

/// Only plain Token-2022 mints: one with any extension (here a mint close
/// authority; a transfer fee would under-deliver deposits) is refused.
#[test]
fn init_vault_rejects_a_mint_with_extensions() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    use spl_token_2022::extension::ExtensionType;
    let mint = Keypair::new();
    let space = ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::MintCloseAuthority]).unwrap();
    let lamports = svm.minimum_balance_for_rent_exemption(space);
    let create = solana_system_interface::instruction::create_account(
        &payer.pubkey(), &mint.pubkey(), lamports, space as u64, &spl_token_2022::ID,
    );
    let close = spl_token_2022::instruction::initialize_mint_close_authority(
        &spl_token_2022::ID, &mint.pubkey(), Some(&payer.pubkey()),
    )
    .unwrap();
    let init = spl_token_2022::instruction::initialize_mint2(&spl_token_2022::ID, &mint.pubkey(), &payer.pubkey(), None, 6).unwrap();
    send(&mut svm, &[create, close, init], &[&payer, &mint]);

    let res = try_send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint.pubkey())], &[&payer]);
    let err = format!("{:?}", res.expect_err("a mint with extensions must be refused"));
    assert!(err.contains(&format!("Custom({})", 0x300F)), "expected MINT_HAS_EXTENSIONS, got {err}");
}

fn verified_depositor_with_deposit(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Address,
    vault_ata: &Address,
) -> (Keypair, Address) {
    let k = Keypair::new();
    svm.airdrop(&k.pubkey(), 1_000_000_000).unwrap();
    send(svm, &[ix_register_account(&payer.pubkey(), &k.pubkey())], &[payer]);
    send(svm, &[ix_update_status(&payer.pubkey(), &k.pubkey(), compliance_status::VERIFIED)], &[payer]);
    let ata = create_user_ata(svm, payer, mint, &k.pubkey());
    mint_to(svm, payer, mint, payer, &ata, 1_000);
    send(svm, &[ix_deposit(&k.pubkey(), &payer.pubkey(), mint, &ata, vault_ata, 1_000)], &[payer, &k]);
    (k, ata)
}

/// A suspended depositor moves their balance to a verified accomplice's
/// position inside the vault. `transfer_within_vault` must refuse, or the
/// accomplice could withdraw it.
#[test]
fn transfer_within_vault_rejects_a_suspended_sender() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    send(&mut svm, &[ix_initialize_registry(&payer.pubkey())], &[&payer]);
    let mint_kp = Keypair::new();
    create_plain_mint(&mut svm, &payer, &mint_kp, &payer.pubkey(), 6);
    let mint = mint_kp.pubkey();
    send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint)], &[&payer]);
    let vault = derive_vault(&mint);
    let vault_ata = create_user_ata(&mut svm, &payer, &mint, &vault);
    let (barred, _) = verified_depositor_with_deposit(&mut svm, &payer, &mint, &vault_ata);
    let (accomplice, _) = verified_depositor_with_deposit(&mut svm, &payer, &mint, &vault_ata);
    send(&mut svm, &[ix_update_status(&payer.pubkey(), &barred.pubkey(), compliance_status::SUSPENDED)], &[&payer]);

    let mut data = vec![vault_ix::TRANSFER_WITHIN_VAULT];
    data.extend_from_slice(&1_000u64.to_le_bytes());
    let ix = Instruction {
        program_id: vault_program_id(),
        accounts: vec![
            AccountMeta::new_readonly(barred.pubkey(), true),
            AccountMeta::new_readonly(vault, false),
            AccountMeta::new(derive_position(&vault, &barred.pubkey()), false),
            AccountMeta::new(derive_position(&vault, &accomplice.pubkey()), false),
            AccountMeta::new_readonly(compliance_program_id(), false),
            AccountMeta::new_readonly(derive_record(&barred.pubkey()), false),
        ],
        data,
    };
    let res = try_send(&mut svm, &[ix], &[&payer, &barred]);
    let err = format!("{:?}", res.expect_err("a suspended sender must not move balance"));
    assert!(err.contains(&format!("Custom({})", 0x3013)), "expected COMPLIANCE_SUSPENDED, got {err}");
}

/// A verified depositor withdraws into someone else's token account. The
/// vault pays out only to the depositor's own account.
#[test]
fn withdraw_rejects_a_destination_not_owned_by_the_depositor() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    send(&mut svm, &[ix_initialize_registry(&payer.pubkey())], &[&payer]);
    let mint_kp = Keypair::new();
    create_plain_mint(&mut svm, &payer, &mint_kp, &payer.pubkey(), 6);
    let mint = mint_kp.pubkey();
    send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint)], &[&payer]);
    let vault = derive_vault(&mint);
    let vault_ata = create_user_ata(&mut svm, &payer, &mint, &vault);
    let (depositor, _) = verified_depositor_with_deposit(&mut svm, &payer, &mint, &vault_ata);
    let outsider_ata = create_user_ata(&mut svm, &payer, &mint, &Keypair::new().pubkey());

    let res = try_send(
        &mut svm,
        &[ix_withdraw(&depositor.pubkey(), &mint, &vault_ata, &outsider_ata, 500)],
        &[&payer, &depositor],
    );
    let err = format!("{:?}", res.expect_err("withdraw to another owner's account must reject"));
    assert!(err.contains(&format!("Custom({})", 0x300E)), "expected TOKEN_ACCOUNT_MISMATCH, got {err}");
    assert_eq!(balance(&svm, &outsider_ata), 0);
}

// ─── Transfer-hook mints ─────────────────────────────────────────────────

/// A Token-2022 mint whose only extension is a transfer hook running
/// `hook_program`, with the payer as hook authority and mint authority.
fn create_hook_mint(svm: &mut LiteSVM, payer: &Keypair, mint_kp: &Keypair, hook_program: Address) {
    use spl_token_2022::extension::{transfer_hook, ExtensionType};
    let space = ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::TransferHook]).unwrap();
    let lamports = svm.minimum_balance_for_rent_exemption(space);
    let create = solana_system_interface::instruction::create_account(
        &payer.pubkey(), &mint_kp.pubkey(), lamports, space as u64, &spl_token_2022::ID,
    );
    let hook = transfer_hook::instruction::initialize(
        &spl_token_2022::ID, &mint_kp.pubkey(), Some(payer.pubkey()), Some(hook_program),
    )
    .unwrap();
    let init = spl_token_2022::instruction::initialize_mint2(&spl_token_2022::ID, &mint_kp.pubkey(), &payer.pubkey(), None, 6).unwrap();
    send(svm, &[create, hook, init], &[payer, mint_kp]);
}

fn derive_meta_list(mint: &Address) -> Address {
    Address::find_program_address(&[seeds::EXTRA_META_LIST, mint.as_ref()], &compliance_program_id()).0
}

fn ix_init_meta_list(payer: &Address, mint: &Address) -> Instruction {
    Instruction {
        program_id: compliance_program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(derive_meta_list(mint), false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(*payer, true),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        ],
        data: psol_compliance::hook_disc::INITIALIZE_EXTRA_ACCOUNT_METAS.to_vec(),
    }
}

/// The accounts `psol_vault::TRANSFER_HOOK_EXTRAS` describes, for a
/// transfer between token accounts owned by `from` and `to`.
fn hook_extras(mint: &Address, from: &Address, to: &Address) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new_readonly(derive_meta_list(mint), false),
        AccountMeta::new_readonly(derive_record(from), false),
        AccountMeta::new_readonly(derive_record(to), false),
        AccountMeta::new_readonly(compliance_program_id(), false),
    ]
}

fn verify(svm: &mut LiteSVM, payer: &Keypair, participant: &Address) {
    send(svm, &[ix_register_account(&payer.pubkey(), participant)], &[payer]);
    send(svm, &[ix_update_status(&payer.pubkey(), participant, compliance_status::VERIFIED)], &[payer]);
}

/// A vault for a mint whose transfer hook is the compliance program, with
/// its meta list, the vault PDA verified (`verify_vault`) and one verified
/// depositor holding 1_000. Returns (mint, vault_ata, depositor, depositor_ata).
fn hook_vault(svm: &mut LiteSVM, payer: &Keypair, verify_vault: bool) -> (Address, Address, Keypair, Address) {
    send(svm, &[ix_initialize_registry(&payer.pubkey())], &[payer]);
    let mint_kp = Keypair::new();
    create_hook_mint(svm, payer, &mint_kp, compliance_program_id());
    let mint = mint_kp.pubkey();
    send(svm, &[ix_init_meta_list(&payer.pubkey(), &mint)], &[payer]);
    send(svm, &[ix_init_vault(&payer.pubkey(), &mint)], &[payer]);
    let vault = derive_vault(&mint);
    if verify_vault {
        verify(svm, payer, &vault);
    }
    let vault_ata = create_user_ata(svm, payer, &mint, &vault);
    let depositor = Keypair::new();
    verify(svm, payer, &depositor.pubkey());
    let depositor_ata = create_user_ata(svm, payer, &mint, &depositor.pubkey());
    mint_to(svm, payer, &mint, payer, &depositor_ata, 1_000);
    (mint, vault_ata, depositor, depositor_ata)
}

fn with_extras(mut ix: Instruction, extras: Vec<AccountMeta>) -> Instruction {
    ix.accounts.extend(extras);
    ix
}

#[test]
fn hook_mint_deposit_and_withdraw_run_the_hook() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    let (mint, vault_ata, depositor, depositor_ata) = hook_vault(&mut svm, &payer, true);
    let d = depositor.pubkey();
    let vault = derive_vault(&mint);

    let deposit = with_extras(ix_deposit(&d, &payer.pubkey(), &mint, &depositor_ata, &vault_ata, 1_000), hook_extras(&mint, &d, &vault));
    send(&mut svm, &[deposit], &[&payer, &depositor]);
    assert_eq!(balance(&svm, &vault_ata), 1_000);

    let withdraw = with_extras(ix_withdraw(&d, &mint, &vault_ata, &depositor_ata, 400), hook_extras(&mint, &vault, &d));
    send(&mut svm, &[withdraw], &[&payer, &depositor]);
    assert_eq!(balance(&svm, &vault_ata), 600);
    assert_eq!(balance(&svm, &depositor_ata), 400);
    let pos = svm.get_account(&derive_position(&vault, &d)).unwrap();
    assert_eq!(from_bytes::<Position>(&pos.data[..Position::LEN]).amount_deposited, 600);
}

/// Without the extra accounts Token-2022 cannot run the hook, and the
/// deposit fails rather than skipping it.
#[test]
fn hook_mint_deposit_without_the_extra_accounts_fails() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    let (mint, vault_ata, depositor, depositor_ata) = hook_vault(&mut svm, &payer, true);
    let res = try_send(
        &mut svm,
        &[ix_deposit(&depositor.pubkey(), &payer.pubkey(), &mint, &depositor_ata, &vault_ata, 1_000)],
        &[&payer, &depositor],
    );
    res.expect_err("a hook mint deposit without the hook's accounts must fail");
    assert_eq!(balance(&svm, &vault_ata), 0);
}

/// The hook checks both owners, and the vault PDA owns one side: with no
/// record for the vault PDA the hook reads an empty account and refuses the
/// deposit (`RECORD_LAYOUT_INVALID`, 0x1010).
#[test]
fn hook_mint_deposit_fails_until_the_vault_pda_is_verified() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    let (mint, vault_ata, depositor, depositor_ata) = hook_vault(&mut svm, &payer, false);
    let d = depositor.pubkey();
    let vault = derive_vault(&mint);
    let deposit = || with_extras(ix_deposit(&d, &payer.pubkey(), &mint, &depositor_ata, &vault_ata, 1_000), hook_extras(&mint, &d, &vault));

    let err = format!("{:?}", try_send(&mut svm, &[deposit()], &[&payer, &depositor]).expect_err("hook must refuse"));
    assert!(err.contains(&format!("Custom({})", 0x1010)), "expected the hook's RECORD_LAYOUT_INVALID, got {err}");

    verify(&mut svm, &payer, &vault);
    svm.expire_blockhash();
    send(&mut svm, &[deposit()], &[&payer, &depositor]);
    assert_eq!(balance(&svm, &vault_ata), 1_000);
}

#[test]
fn init_vault_rejects_a_hook_running_another_program() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    let mint_kp = Keypair::new();
    create_hook_mint(&mut svm, &payer, &mint_kp, vault_program_id());
    let res = try_send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint_kp.pubkey())], &[&payer]);
    let err = format!("{:?}", res.expect_err("a hook running another program must be refused"));
    assert!(err.contains(&format!("Custom({})", 0x3050)), "expected TRANSFER_HOOK_MISMATCH, got {err}");
}

/// The hook authority can point the hook at another program after
/// `init_vault`; deposits must notice.
#[test]
fn deposit_rejects_after_the_hook_program_is_changed() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    let (mint, vault_ata, depositor, depositor_ata) = hook_vault(&mut svm, &payer, true);
    let update = spl_token_2022::extension::transfer_hook::instruction::update(
        &spl_token_2022::ID, &mint, &payer.pubkey(), &[], Some(vault_program_id()),
    )
    .unwrap();
    send(&mut svm, &[update], &[&payer]);

    let d = depositor.pubkey();
    let deposit = with_extras(
        ix_deposit(&d, &payer.pubkey(), &mint, &depositor_ata, &vault_ata, 1_000),
        hook_extras(&mint, &d, &derive_vault(&mint)),
    );
    let err = format!("{:?}", try_send(&mut svm, &[deposit], &[&payer, &depositor]).expect_err("must refuse"));
    assert!(err.contains(&format!("Custom({})", 0x3050)), "expected TRANSFER_HOOK_MISMATCH, got {err}");
}

#[test]
fn init_vault_rejects_a_hook_mint_with_another_extension() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    use spl_token_2022::extension::{transfer_hook, ExtensionType};
    let mint = Keypair::new();
    let space = ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::TransferHook, ExtensionType::MintCloseAuthority]).unwrap();
    let lamports = svm.minimum_balance_for_rent_exemption(space);
    let create = solana_system_interface::instruction::create_account(
        &payer.pubkey(), &mint.pubkey(), lamports, space as u64, &spl_token_2022::ID,
    );
    let hook = transfer_hook::instruction::initialize(
        &spl_token_2022::ID, &mint.pubkey(), Some(payer.pubkey()), Some(compliance_program_id()),
    )
    .unwrap();
    let close = spl_token_2022::instruction::initialize_mint_close_authority(
        &spl_token_2022::ID, &mint.pubkey(), Some(&payer.pubkey()),
    )
    .unwrap();
    let init = spl_token_2022::instruction::initialize_mint2(&spl_token_2022::ID, &mint.pubkey(), &payer.pubkey(), None, 6).unwrap();
    send(&mut svm, &[create, hook, close, init], &[&payer, &mint]);

    let res = try_send(&mut svm, &[ix_init_vault(&payer.pubkey(), &mint.pubkey())], &[&payer]);
    let err = format!("{:?}", res.expect_err("a second extension must be refused"));
    assert!(err.contains(&format!("Custom({})", 0x300F)), "expected MINT_HAS_EXTENSIONS, got {err}");
}

/// Withdrawals check the hook too: without the extra accounts, and after
/// the hook authority points the hook elsewhere (funds stay put until it
/// points back).
#[test]
fn hook_mint_withdraw_needs_the_extras_and_the_registry_hook() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    let (mint, vault_ata, depositor, depositor_ata) = hook_vault(&mut svm, &payer, true);
    let d = depositor.pubkey();
    let vault = derive_vault(&mint);
    let deposit = with_extras(ix_deposit(&d, &payer.pubkey(), &mint, &depositor_ata, &vault_ata, 1_000), hook_extras(&mint, &d, &vault));
    send(&mut svm, &[deposit], &[&payer, &depositor]);

    try_send(&mut svm, &[ix_withdraw(&d, &mint, &vault_ata, &depositor_ata, 400)], &[&payer, &depositor])
        .expect_err("withdraw without the hook's accounts must fail");

    let repoint = |svm: &mut LiteSVM, program: Option<Address>| {
        let ix = spl_token_2022::extension::transfer_hook::instruction::update(&spl_token_2022::ID, &mint, &payer.pubkey(), &[], program).unwrap();
        svm.expire_blockhash();
        send(svm, &[ix], &[&payer]);
    };
    let withdraw = || with_extras(ix_withdraw(&d, &mint, &vault_ata, &depositor_ata, 400), hook_extras(&mint, &vault, &d));
    repoint(&mut svm, Some(vault_program_id()));
    let err = format!("{:?}", try_send(&mut svm, &[withdraw()], &[&payer, &depositor]).expect_err("must refuse"));
    assert!(err.contains(&format!("Custom({})", 0x3050)), "expected TRANSFER_HOOK_MISMATCH, got {err}");

    repoint(&mut svm, Some(compliance_program_id()));
    send(&mut svm, &[withdraw()], &[&payer, &depositor]);
    assert_eq!(balance(&svm, &depositor_ata), 400);
}

/// Clearing the hook would let transfers skip it, and with it the vault
/// PDA's record. Deposits and withdrawals refuse a cleared hook as they
/// refuse a repointed one, and work again once it points back.
#[test]
fn hook_mint_deposit_and_withdraw_reject_a_cleared_hook() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    let (mint, vault_ata, depositor, depositor_ata) = hook_vault(&mut svm, &payer, true);
    let d = depositor.pubkey();
    let vault = derive_vault(&mint);
    let deposit = |amount| with_extras(ix_deposit(&d, &payer.pubkey(), &mint, &depositor_ata, &vault_ata, amount), hook_extras(&mint, &d, &vault));
    send(&mut svm, &[deposit(600)], &[&payer, &depositor]);

    let repoint = |svm: &mut LiteSVM, program: Option<Address>| {
        let ix = spl_token_2022::extension::transfer_hook::instruction::update(&spl_token_2022::ID, &mint, &payer.pubkey(), &[], program).unwrap();
        svm.expire_blockhash();
        send(svm, &[ix], &[&payer]);
    };
    repoint(&mut svm, None);
    let withdraw = || with_extras(ix_withdraw(&d, &mint, &vault_ata, &depositor_ata, 400), hook_extras(&mint, &vault, &d));
    for (label, ix) in [
        ("deposit", deposit(400)),
        ("deposit without extras", ix_deposit(&d, &payer.pubkey(), &mint, &depositor_ata, &vault_ata, 400)),
        ("withdraw", withdraw()),
        ("withdraw without extras", ix_withdraw(&d, &mint, &vault_ata, &depositor_ata, 400)),
    ] {
        let err = format!("{:?}", try_send(&mut svm, &[ix], &[&payer, &depositor]).expect_err(label));
        assert!(err.contains(&format!("Custom({})", 0x3050)), "{label}: expected TRANSFER_HOOK_MISMATCH, got {err}");
    }
    assert_eq!(balance(&svm, &vault_ata), 600);

    repoint(&mut svm, Some(compliance_program_id()));
    send(&mut svm, &[withdraw()], &[&payer, &depositor]);
    assert_eq!(balance(&svm, &depositor_ata), 800);
}

/// The vault forwards whatever extra accounts the caller passes, so the
/// caller can't be trusted to pass the right ones. Token-2022 resolves the
/// hook's accounts itself; substituting any of them must fail, not run the
/// hook against a record the caller chose. Here the vault PDA is not
/// verified, so the honest deposit is refused and every substitution tries
/// to get it through.
#[test]
fn hook_mint_deposit_rejects_substituted_extra_accounts() {
    let Some((mut svm, payer)) = setup() else {
        return;
    };
    let (mint, vault_ata, depositor, depositor_ata) = hook_vault(&mut svm, &payer, false);
    let d = depositor.pubkey();
    let vault = derive_vault(&mint);
    // Another mint with its own meta list, and another verified participant.
    let other_mint = Keypair::new();
    create_hook_mint(&mut svm, &payer, &other_mint, compliance_program_id());
    send(&mut svm, &[ix_init_meta_list(&payer.pubkey(), &other_mint.pubkey())], &[&payer]);
    let bystander = Keypair::new().pubkey();
    verify(&mut svm, &payer, &bystander);

    let honest = hook_extras(&mint, &d, &vault);
    let swap = |i: usize, key: Address| {
        let mut e = honest.clone();
        e[i] = AccountMeta::new_readonly(key, false);
        e
    };
    let cases = [
        ("vault record -> bystander's", swap(2, derive_record(&bystander))),
        ("vault record -> depositor's", swap(2, derive_record(&d))),
        ("meta list -> another mint's", swap(0, derive_meta_list(&other_mint.pubkey()))),
        ("hook program -> vault program", swap(3, vault_program_id())),
        ("extras reversed", honest.iter().rev().cloned().collect()),
    ];
    for (label, extras) in cases {
        svm.expire_blockhash();
        let ix = with_extras(ix_deposit(&d, &payer.pubkey(), &mint, &depositor_ata, &vault_ata, 1_000), extras);
        try_send(&mut svm, &[ix], &[&payer, &depositor]).expect_err(label);
        assert_eq!(balance(&svm, &vault_ata), 0, "{label}");
    }
}
