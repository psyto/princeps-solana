//! Shared harness for the quickstarts: load the parts' SBF builds into
//! LiteSVM, and build their instructions. Each builder documents the
//! account order the program expects, so this file doubles as a
//! reference for calling the programs from your own client.

#![allow(dead_code)]

use {
    litesvm::LiteSVM,
    solana_address::Address,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_message::Message,
    solana_program_pack::Pack,
    solana_signer::Signer,
    solana_transaction::Transaction,
    spl_token_2022::{
        extension::{transfer_hook, ExtensionType},
        state::{Account as TokenAccount, Mint},
    },
    psol_types::{compliance_status, seeds},
    std::path::PathBuf,
};

// Program ids for this local run. A real deployment uses its own.
pub const COMPLIANCE: Address = Address::new_from_array([7; 32]);
pub const VAULT: Address = Address::new_from_array([9; 32]);
pub const SETTLEMENT: Address = Address::new_from_array([10; 32]);
pub const LENDING: Address = Address::new_from_array([11; 32]);

const SYSTEM: Address = solana_system_interface::program::ID;
const ATA_PROGRAM: Address = Address::new_from_array(pinocchio_pubkey::from_str(
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
));
fn token_2022() -> Address {
    spl_token_2022::ID.to_bytes().into()
}

// ─── Harness ─────────────────────────────────────────────────────────────

pub struct Env {
    pub svm: LiteSVM,
    /// Compliance super-admin and operator for every role, and payer.
    pub admin: Keypair,
}

/// Load `target/deploy/<name>.so` for each `(name, id)` into a fresh LiteSVM.
pub fn env(programs: &[(&str, Address)]) -> Env {
    let mut svm = LiteSVM::new();
    for (name, id) in programs {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push(format!("../target/deploy/{name}.so"));
        let bytes = std::fs::read(&path).unwrap_or_else(|_| {
            panic!(
                "{} not found. Build the programs first: ./scripts/build-sbf.sh",
                path.display()
            )
        });
        svm.add_program(*id, &bytes).unwrap();
    }
    let admin = Keypair::new();
    svm.airdrop(&admin.pubkey(), 100_000_000_000).unwrap();
    Env { svm, admin }
}

impl Env {
    /// Send `ixs`, the admin paying. On failure, return the program's
    /// custom error code as `0x....` (or the raw error if it has none).
    pub fn send(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), String> {
        let mut all: Vec<&Keypair> = vec![&self.admin];
        all.extend(signers.iter().copied().filter(|k| k.pubkey() != self.admin.pubkey()));
        let msg = Message::new(ixs, Some(&self.admin.pubkey()));
        let tx = Transaction::new(&all, msg, self.svm.latest_blockhash());
        let res = self.svm.send_transaction(tx);
        // A retry of the same instructions must not look like a duplicate.
        self.svm.expire_blockhash();
        res.map(|_| ()).map_err(|e| {
            let raw = format!("{:?}", e.err);
            match raw.split("Custom(").nth(1).and_then(|s| s.split(')').next()) {
                Some(n) => format!("0x{:04X}", n.parse::<u32>().unwrap()),
                None => raw,
            }
        })
    }

    /// `send`, for printing: shows as `ok` or `refused 0x....`.
    pub fn attempt(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> Outcome {
        Outcome(self.send(ixs, signers))
    }

    pub fn ok(&mut self, ixs: &[Instruction], signers: &[&Keypair]) {
        self.send(ixs, signers).expect("transaction failed");
    }

    pub fn user(&mut self) -> Keypair {
        let k = Keypair::new();
        self.svm.airdrop(&k.pubkey(), 1_000_000_000).unwrap();
        k
    }

    pub fn balance(&self, token_account: &Address) -> u64 {
        let acc = self.svm.get_account(token_account).unwrap();
        TokenAccount::unpack(&acc.data[..TokenAccount::LEN]).unwrap().amount
    }
}

pub struct Outcome(pub Result<(), String>);

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Ok(()) => write!(f, "ok"),
            Err(code) => write!(f, "refused {code}"),
        }
    }
}

// ─── psol-compliance ─────────────────────────────────────────────────────

pub fn registry() -> Address {
    Address::find_program_address(&[seeds::REGISTRY], &COMPLIANCE).0
}
pub fn record(participant: &Address) -> Address {
    Address::find_program_address(&[seeds::ACCOUNT_RECORD, participant.as_ref()], &COMPLIANCE).0
}

/// Accounts: [payer+super-admin (s,w), registry PDA (w), system].
pub fn init_registry(admin: &Address) -> Instruction {
    Instruction {
        program_id: COMPLIANCE,
        accounts: vec![
            AccountMeta::new(*admin, true),
            AccountMeta::new(registry(), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
        data: vec![psol_compliance::ix::INITIALIZE_REGISTRY],
    }
}

/// Accounts: [onboard operator (s), payer (s,w), registry, record PDA (w), system].
/// Data: participant(32) jurisdiction(2). The record starts PENDING.
pub fn register(admin: &Address, participant: &Address) -> Instruction {
    let mut data = vec![psol_compliance::ix::REGISTER_ACCOUNT];
    data.extend_from_slice(participant.as_ref());
    data.extend_from_slice(b"JP");
    Instruction {
        program_id: COMPLIANCE,
        accounts: vec![
            AccountMeta::new_readonly(*admin, true),
            AccountMeta::new(*admin, true),
            AccountMeta::new_readonly(registry(), false),
            AccountMeta::new(record(participant), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
        data,
    }
}

/// Accounts: [status operator (s), registry, record (w)].
/// Data: status, flags, change mask.
pub fn set_status(admin: &Address, participant: &Address, status: u8) -> Instruction {
    Instruction {
        program_id: COMPLIANCE,
        accounts: vec![
            AccountMeta::new_readonly(*admin, true),
            AccountMeta::new_readonly(registry(), false),
            AccountMeta::new(record(participant), false),
        ],
        data: vec![
            psol_compliance::ix::UPDATE_STATUS,
            status,
            0,
            psol_compliance::change_mask::STATUS,
        ],
    }
}

/// Register `participant` and mark them VERIFIED.
pub fn onboard(env: &mut Env, participant: &Address) {
    let a = env.admin.pubkey();
    env.ok(&[register(&a, participant), set_status(&a, participant, compliance_status::VERIFIED)], &[]);
}

pub fn risk_params() -> Address {
    Address::find_program_address(&[seeds::RISK_PARAMS], &COMPLIANCE).0
}
pub fn price_feed(mint: &Address) -> Address {
    Address::find_program_address(&[seeds::PRICE_FEED, mint.as_ref()], &COMPLIANCE).0
}

/// Accounts: [super-admin (s), payer (s,w), registry, risk params PDA (w), system].
/// Creates the haircut table with its defaults (`psol_types::DEFAULT_HAIRCUTS`).
pub fn init_risk_params(admin: &Address) -> Instruction {
    Instruction {
        program_id: COMPLIANCE,
        accounts: vec![
            AccountMeta::new_readonly(*admin, true),
            AccountMeta::new(*admin, true),
            AccountMeta::new_readonly(registry(), false),
            AccountMeta::new(risk_params(), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
        data: vec![psol_compliance::ix::INITIALIZE_RISK_PARAMS],
    }
}

/// Accounts: [oracle operator (s), payer (s,w), registry, feed PDA (w), system].
/// Data: mint(32) price in micro-USD(8) mint decimals(1).
pub fn register_price(admin: &Address, mint: &Address, micro_usd: u64, decimals: u8) -> Instruction {
    let mut data = vec![psol_compliance::ix::REGISTER_PRICE_FEED];
    data.extend_from_slice(mint.as_ref());
    data.extend_from_slice(&micro_usd.to_le_bytes());
    data.push(decimals);
    Instruction {
        program_id: COMPLIANCE,
        accounts: vec![
            AccountMeta::new_readonly(*admin, true),
            AccountMeta::new(*admin, true),
            AccountMeta::new_readonly(registry(), false),
            AccountMeta::new(price_feed(mint), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
        data,
    }
}

// ─── Token-2022 ──────────────────────────────────────────────────────────

/// A plain Token-2022 mint; the admin is the mint authority.
pub fn plain_mint(env: &mut Env, decimals: u8) -> Address {
    let mint = Keypair::new();
    let lamports = env.svm.minimum_balance_for_rent_exemption(Mint::LEN);
    let a = env.admin.pubkey();
    let create = solana_system_interface::instruction::create_account(
        &a, &mint.pubkey(), lamports, Mint::LEN as u64, &spl_token_2022::ID,
    );
    let init = spl_token_2022::instruction::initialize_mint2(
        &spl_token_2022::ID, &mint.pubkey(), &a, None, decimals,
    )
    .unwrap();
    env.ok(&[create, init], &[&mint]);
    mint.pubkey()
}

pub fn ata_address(owner: &Address, mint: &Address) -> Address {
    Address::find_program_address(&[owner.as_ref(), token_2022().as_ref(), mint.as_ref()], &ATA_PROGRAM).0
}

/// Create (idempotently) `owner`'s associated token account for `mint`.
pub fn ata(env: &mut Env, mint: &Address, owner: &Address) -> Address {
    let ata = ata_address(owner, mint);
    let ix = Instruction {
        program_id: ATA_PROGRAM,
        accounts: vec![
            AccountMeta::new(env.admin.pubkey(), true),
            AccountMeta::new(ata, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(SYSTEM, false),
            AccountMeta::new_readonly(token_2022(), false),
        ],
        data: vec![1], // CreateIdempotent
    };
    env.ok(&[ix], &[]);
    ata
}

pub fn mint_to(env: &mut Env, mint: &Address, dest: &Address, amount: u64) {
    let ix = spl_token_2022::instruction::mint_to(
        &spl_token_2022::ID, mint, dest, &env.admin.pubkey(), &[], amount,
    )
    .unwrap();
    env.ok(&[ix], &[]);
}

// ─── psol-compliance as a Token-2022 transfer hook ───────────────────────

pub fn meta_list(mint: &Address) -> Address {
    Address::find_program_address(&[seeds::EXTRA_META_LIST, mint.as_ref()], &COMPLIANCE).0
}

/// A Token-2022 mint whose `TransferHook` points at the compliance
/// program, plus the `ExtraAccountMetaList` the hook needs.
pub fn hooked_mint(env: &mut Env, decimals: u8) -> Address {
    let mint = Keypair::new();
    let a = env.admin.pubkey();
    let space = ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::TransferHook]).unwrap();
    let lamports = env.svm.minimum_balance_for_rent_exemption(space);
    let create = solana_system_interface::instruction::create_account(
        &a, &mint.pubkey(), lamports, space as u64, &spl_token_2022::ID,
    );
    let hook = transfer_hook::instruction::initialize(
        &spl_token_2022::ID, &mint.pubkey(), Some(a), Some(COMPLIANCE),
    )
    .unwrap();
    let init = spl_token_2022::instruction::initialize_mint2(
        &spl_token_2022::ID, &mint.pubkey(), &a, None, decimals,
    )
    .unwrap();
    env.ok(&[create, hook, init], &[&mint]);

    // Accounts: [payer (s,w), meta list PDA (w), mint, mint authority (s), system].
    let metas = Instruction {
        program_id: COMPLIANCE,
        accounts: vec![
            AccountMeta::new(a, true),
            AccountMeta::new(meta_list(&mint.pubkey()), false),
            AccountMeta::new_readonly(mint.pubkey(), false),
            AccountMeta::new_readonly(a, true),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
        data: psol_compliance::hook_disc::INITIALIZE_EXTRA_ACCOUNT_METAS.to_vec(),
    };
    env.ok(&[metas], &[]);
    mint.pubkey()
}

/// A token account for a hooked mint (it needs the `TransferHookAccount`
/// extension, so the plain ATA flow is not used here).
pub fn hooked_token_account(env: &mut Env, mint: &Address, owner: &Address) -> Address {
    let acct = Keypair::new();
    let space =
        ExtensionType::try_calculate_account_len::<TokenAccount>(&[ExtensionType::TransferHookAccount])
            .unwrap();
    let lamports = env.svm.minimum_balance_for_rent_exemption(space);
    let create = solana_system_interface::instruction::create_account(
        &env.admin.pubkey(), &acct.pubkey(), lamports, space as u64, &spl_token_2022::ID,
    );
    let init =
        spl_token_2022::instruction::initialize_account3(&spl_token_2022::ID, &acct.pubkey(), mint, owner)
            .unwrap();
    env.ok(&[create, init], &[&acct]);
    acct.pubkey()
}

/// `transfer_checked` with the hook's extra accounts appended: both
/// parties' records, the hook program, and the meta list.
pub fn hooked_transfer(
    from: &Address,
    from_owner: &Address,
    to: &Address,
    to_owner: &Address,
    mint: &Address,
    amount: u64,
    decimals: u8,
) -> Instruction {
    let mut ix = spl_token_2022::instruction::transfer_checked(
        &spl_token_2022::ID, from, mint, to, from_owner, &[], amount, decimals,
    )
    .unwrap();
    ix.accounts.push(AccountMeta::new_readonly(record(from_owner), false));
    ix.accounts.push(AccountMeta::new_readonly(record(to_owner), false));
    ix.accounts.push(AccountMeta::new_readonly(COMPLIANCE, false));
    ix.accounts.push(AccountMeta::new_readonly(meta_list(mint), false));
    ix
}

// ─── psol-vault ──────────────────────────────────────────────────────────

pub fn vault(mint: &Address) -> Address {
    Address::find_program_address(&[seeds::VAULT, mint.as_ref()], &VAULT).0
}
pub fn position(vault: &Address, depositor: &Address) -> Address {
    Address::find_program_address(&[seeds::POSITION, vault.as_ref(), depositor.as_ref()], &VAULT).0
}

/// Accounts: [admin (s,w), vault PDA (w), mint, system, compliance program].
/// Data: asset class. Also creates the vault's token account.
pub fn init_vault(env: &mut Env, mint: &Address, asset_class: u8) -> Address {
    let v = vault(mint);
    let ix = Instruction {
        program_id: VAULT,
        accounts: vec![
            AccountMeta::new(env.admin.pubkey(), true),
            AccountMeta::new(v, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(SYSTEM, false),
            AccountMeta::new_readonly(COMPLIANCE, false),
        ],
        data: vec![psol_vault::ix::INIT_VAULT, asset_class],
    };
    env.ok(&[ix], &[]);
    ata(env, mint, &v);
    v
}

/// Accounts: [depositor (s), payer (s,w), depositor's record, compliance
/// program, vault (w), position (w), mint, depositor ATA (w), vault ATA (w),
/// token program, system]. Data: amount.
pub fn deposit(payer: &Address, depositor: &Address, mint: &Address, amount: u64) -> Instruction {
    let v = vault(mint);
    let mut data = vec![psol_vault::ix::DEPOSIT];
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: VAULT,
        accounts: vec![
            AccountMeta::new_readonly(*depositor, true),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(record(depositor), false),
            AccountMeta::new_readonly(COMPLIANCE, false),
            AccountMeta::new(v, false),
            AccountMeta::new(position(&v, depositor), false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(ata_address(depositor, mint), false),
            AccountMeta::new(ata_address(&v, mint), false),
            AccountMeta::new_readonly(token_2022(), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
        data,
    }
}

/// Accounts: [depositor (s), depositor's record, compliance program,
/// vault (w), position (w), mint, vault ATA (w), depositor ATA (w),
/// token program]. Data: amount.
pub fn withdraw(depositor: &Address, mint: &Address, amount: u64) -> Instruction {
    let v = vault(mint);
    let mut data = vec![psol_vault::ix::WITHDRAW];
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: VAULT,
        accounts: vec![
            AccountMeta::new_readonly(*depositor, true),
            AccountMeta::new_readonly(record(depositor), false),
            AccountMeta::new_readonly(COMPLIANCE, false),
            AccountMeta::new(v, false),
            AccountMeta::new(position(&v, depositor), false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(ata_address(&v, mint), false),
            AccountMeta::new(ata_address(depositor, mint), false),
            AccountMeta::new_readonly(token_2022(), false),
        ],
        data,
    }
}

/// Accounts: [depositor (s), payer (s,w), vault, position (w), system].
/// An empty position, so a transfer inside the vault can credit it.
pub fn init_position(payer: &Address, depositor: &Address, mint: &Address) -> Instruction {
    let v = vault(mint);
    Instruction {
        program_id: VAULT,
        accounts: vec![
            AccountMeta::new_readonly(*depositor, true),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(v, false),
            AccountMeta::new(position(&v, depositor), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
        data: vec![psol_vault::ix::INIT_POSITION],
    }
}

// ─── psol-settlement ─────────────────────────────────────────────────────

pub struct Swap {
    pub a: Address,
    pub b: Address,
    pub mint_a: Address,
    pub mint_b: Address,
    pub amount_a: u64,
    pub amount_b: u64,
    pub decimals: u8,
    pub nonce: u64,
}

impl Swap {
    /// The ticket PDA. Its seeds include the compliance program, so the
    /// address both parties expect commits to the registry that gates it.
    pub fn ticket(&self) -> Address {
        Address::find_program_address(
            &[
                psol_settlement::DVP,
                self.a.as_ref(),
                self.b.as_ref(),
                self.mint_a.as_ref(),
                self.mint_b.as_ref(),
                &self.nonce.to_le_bytes(),
                COMPLIANCE.as_ref(),
            ],
            &SETTLEMENT,
        )
        .0
    }

    /// Accounts: [payer (s,w), ticket PDA (w), system, compliance program,
    /// mint A, mint B].
    /// Data: a b mint_a mint_b amount_a amount_b decimals_a decimals_b
    /// expiry_slot nonce. Also creates the ticket's two escrow ATAs.
    pub fn create(&self, env: &mut Env, expiry_slot: u64) {
        let mut data = vec![psol_settlement::ix::CREATE_SWAP];
        for k in [&self.a, &self.b, &self.mint_a, &self.mint_b] {
            data.extend_from_slice(k.as_ref());
        }
        data.extend_from_slice(&self.amount_a.to_le_bytes());
        data.extend_from_slice(&self.amount_b.to_le_bytes());
        data.extend_from_slice(&[self.decimals, self.decimals]);
        data.extend_from_slice(&expiry_slot.to_le_bytes());
        data.extend_from_slice(&self.nonce.to_le_bytes());
        let ix = Instruction {
            program_id: SETTLEMENT,
            accounts: vec![
                AccountMeta::new(env.admin.pubkey(), true),
                AccountMeta::new(self.ticket(), false),
                AccountMeta::new_readonly(SYSTEM, false),
                AccountMeta::new_readonly(COMPLIANCE, false),
                AccountMeta::new_readonly(self.mint_a, false),
                AccountMeta::new_readonly(self.mint_b, false),
            ],
            data,
        };
        env.ok(&[ix], &[]);
        let t = self.ticket();
        ata(env, &self.mint_a, &t);
        ata(env, &self.mint_b, &t);
    }

    /// Accounts: [funder (s), ticket (w), funder's ATA (w), escrow ATA (w),
    /// mint, token program]. The funder's leg moves into escrow.
    pub fn fund(&self, funder: &Address) -> Instruction {
        let mint = if funder == &self.a { self.mint_a } else { self.mint_b };
        let t = self.ticket();
        Instruction {
            program_id: SETTLEMENT,
            accounts: vec![
                AccountMeta::new_readonly(*funder, true),
                AccountMeta::new(t, false),
                AccountMeta::new(ata_address(funder, &mint), false),
                AccountMeta::new(ata_address(&t, &mint), false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(token_2022(), false),
            ],
            data: vec![psol_settlement::ix::FUND],
        }
    }

    /// Accounts: [ticket (w), escrow A (w), escrow B (w), a's ATA for mint B
    /// (w), b's ATA for mint A (w), mint A, mint B, a's record, b's record,
    /// compliance program, token program]. Anyone may call it.
    pub fn settle(&self) -> Instruction {
        let t = self.ticket();
        Instruction {
            program_id: SETTLEMENT,
            accounts: vec![
                AccountMeta::new(t, false),
                AccountMeta::new(ata_address(&t, &self.mint_a), false),
                AccountMeta::new(ata_address(&t, &self.mint_b), false),
                AccountMeta::new(ata_address(&self.a, &self.mint_b), false),
                AccountMeta::new(ata_address(&self.b, &self.mint_a), false),
                AccountMeta::new_readonly(self.mint_a, false),
                AccountMeta::new_readonly(self.mint_b, false),
                AccountMeta::new_readonly(record(&self.a), false),
                AccountMeta::new_readonly(record(&self.b), false),
                AccountMeta::new_readonly(COMPLIANCE, false),
                AccountMeta::new_readonly(token_2022(), false),
            ],
            data: vec![psol_settlement::ix::SETTLE],
        }
    }
}

// ─── psol-lending ────────────────────────────────────────────────────────

pub fn loan(borrower: &Address, lender: &Address, collateral_mint: &Address, cash_mint: &Address, nonce: u64) -> Address {
    Address::find_program_address(
        &[
            seeds::LOAN,
            borrower.as_ref(),
            lender.as_ref(),
            vault(collateral_mint).as_ref(),
            vault(cash_mint).as_ref(),
            &nonce.to_le_bytes(),
        ],
        &LENDING,
    )
    .0
}
pub fn loan_list(borrower: &Address) -> Address {
    Address::find_program_address(&[seeds::LOAN_LIST, borrower.as_ref()], &LENDING).0
}

/// A borrower's first loan, with no other positions or loans disclosed.
/// Accounts: [borrower (s), lender (s), payer (s,w), borrower record, lender
/// record, compliance program, vault program, collateral vault, borrower's
/// collateral position (w), cash vault, lender's cash position (w),
/// borrower's cash position (w), loan PDA (w), system, loan list (w), risk
/// params, then price feeds in ascending address order].
/// Data: collateral, principal, maturity slot, nonce, interest bps/year,
/// #extra positions, #price feeds, #existing loans.
#[allow(clippy::too_many_arguments)]
pub fn open_loan(
    payer: &Address,
    borrower: &Address,
    lender: &Address,
    collateral_mint: &Address,
    cash_mint: &Address,
    collateral: u64,
    principal: u64,
    nonce: u64,
) -> Instruction {
    let cv = vault(collateral_mint);
    let kv = vault(cash_mint);
    let mut data = vec![psol_lending::ix::OPEN_LOAN];
    data.extend_from_slice(&collateral.to_le_bytes());
    data.extend_from_slice(&principal.to_le_bytes());
    data.extend_from_slice(&u64::MAX.to_le_bytes());
    data.extend_from_slice(&nonce.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&[0, 2, 0]);
    let mut feeds = [price_feed(collateral_mint), price_feed(cash_mint)];
    feeds.sort();
    let mut accounts = vec![
        AccountMeta::new_readonly(*borrower, true),
        AccountMeta::new_readonly(*lender, true),
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(record(borrower), false),
        AccountMeta::new_readonly(record(lender), false),
        AccountMeta::new_readonly(COMPLIANCE, false),
        AccountMeta::new_readonly(VAULT, false),
        AccountMeta::new_readonly(cv, false),
        AccountMeta::new(position(&cv, borrower), false),
        AccountMeta::new_readonly(kv, false),
        AccountMeta::new(position(&kv, lender), false),
        AccountMeta::new(position(&kv, borrower), false),
        AccountMeta::new(loan(borrower, lender, collateral_mint, cash_mint, nonce), false),
        AccountMeta::new_readonly(SYSTEM, false),
        AccountMeta::new(loan_list(borrower), false),
        AccountMeta::new_readonly(risk_params(), false),
    ];
    accounts.extend(feeds.iter().map(|f| AccountMeta::new_readonly(*f, false)));
    Instruction { program_id: LENDING, accounts, data }
}

/// Accounts: [borrower (s), vault program, collateral vault, borrower's
/// collateral position (w), cash vault, borrower's cash position (w),
/// lender's cash position (w), loan (w), loan list (w), compliance program,
/// borrower's record].
pub fn repay_loan(
    borrower: &Address,
    lender: &Address,
    collateral_mint: &Address,
    cash_mint: &Address,
    nonce: u64,
) -> Instruction {
    let cv = vault(collateral_mint);
    let kv = vault(cash_mint);
    Instruction {
        program_id: LENDING,
        accounts: vec![
            AccountMeta::new_readonly(*borrower, true),
            AccountMeta::new_readonly(VAULT, false),
            AccountMeta::new_readonly(cv, false),
            AccountMeta::new(position(&cv, borrower), false),
            AccountMeta::new_readonly(kv, false),
            AccountMeta::new(position(&kv, borrower), false),
            AccountMeta::new(position(&kv, lender), false),
            AccountMeta::new(loan(borrower, lender, collateral_mint, cash_mint, nonce), false),
            AccountMeta::new(loan_list(borrower), false),
            AccountMeta::new_readonly(COMPLIANCE, false),
            AccountMeta::new_readonly(record(borrower), false),
        ],
        data: vec![psol_lending::ix::REPAY_LOAN],
    }
}

/// Read a `Position` from the vault program.
pub fn read_position(env: &Env, vault: &Address, depositor: &Address) -> psol_types::Position {
    let acc = env.svm.get_account(&position(vault, depositor)).unwrap();
    *bytemuck::from_bytes(&acc.data[..psol_types::Position::LEN])
}
