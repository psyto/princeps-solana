//! `psol-settlement` — self-contained atomic delivery-vs-payment (DvP).
//!
//! Two verified parties atomically swap an asset leg for a cash leg. Neither
//! leg moves unless BOTH are funded, both parties pass compliance, and the
//! ticket has not expired. This is a fully self-contained escrow: the
//! `SwapTicket` PDA is the settlement authority and the escrow token
//! accounts are owned by it — no external settlement channel is required.
//!
//! Instructions:
//!   * `create_swap` — initialize a `SwapTicket` PDA for (a, b, mint_a, mint_b, nonce).
//!   * `fund`        — a party moves its leg into the ticket's escrow ATA.
//!   * `settle`      — after both funded + compliance + not expired, atomically
//!                     deliver escrow_a → party_b and escrow_b → party_a (PDA-signed).
//!   * `reclaim`     — after expiry (and not settled), refund each funded leg.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use bytemuck::{Pod, Zeroable};
use pinocchio::{
    ProgramResult, account_info::AccountInfo,
    instruction::{AccountMeta, Instruction, Seed, Signer},
    msg,
    program_error::ProgramError,
    pubkey::{Pubkey, find_program_address},
    sysvars::{Sysvar, clock::Clock, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;
use psol_types::{CheckError, read_account_record};

pinocchio::program_entrypoint!(process_instruction);
pinocchio::default_allocator!();
pinocchio::nostd_panic_handler!();

// ─── Constants ─────────────────────────────────────────────────────────────

/// Token-2022 program ID.
pub const TOKEN_2022_PROGRAM_ID: Pubkey =
    pinocchio_pubkey::from_str("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
/// Token-2022 `TransferChecked` instruction discriminator.
const TOKEN_IX_TRANSFER_CHECKED: u8 = 12;
/// `TransferChecked` data payload: tag(1) + amount(8) + decimals(1).
const TRANSFER_CHECKED_DATA_LEN: usize = 10;

/// PDA seed prefix for a swap ticket.
pub const DVP: &[u8] = b"dvp";

pub mod ix {
    /// Initialize a `SwapTicket`.
    pub const CREATE_SWAP: u8 = 0;
    /// A party funds its leg into escrow.
    pub const FUND: u8 = 1;
    /// Atomically deliver both legs after compliance passes.
    pub const SETTLE: u8 = 2;
    /// Refund funded legs after expiry.
    pub const RECLAIM: u8 = 3;
}

// 0x2000-0x2FFF band (disjoint from compliance 0x1xxx / vault 0x3xxx).
pub mod err {
    use pinocchio::program_error::ProgramError;
    pub const TICKET_PDA_MISMATCH: ProgramError = ProgramError::Custom(0x2001);
    pub const NOT_A_PARTY: ProgramError = ProgramError::Custom(0x2002);
    pub const ALREADY_FUNDED: ProgramError = ProgramError::Custom(0x2003);
    pub const NOT_FULLY_FUNDED: ProgramError = ProgramError::Custom(0x2004);
    pub const ALREADY_SETTLED: ProgramError = ProgramError::Custom(0x2005);
    pub const EXPIRED: ProgramError = ProgramError::Custom(0x2006);
    pub const NOT_EXPIRED: ProgramError = ProgramError::Custom(0x2007);
    pub const TOKEN_PROGRAM_MISMATCH: ProgramError = ProgramError::Custom(0x2008);
    pub const TICKET_LAYOUT_INVALID: ProgramError = ProgramError::Custom(0x2009);
    pub const MINT_MISMATCH: ProgramError = ProgramError::Custom(0x200A);
    pub const DATA_TOO_SHORT: ProgramError = ProgramError::Custom(0x200B);
    pub const NOTHING_TO_RECLAIM: ProgramError = ProgramError::Custom(0x200C);
    // Compliance failures (mirror `CheckError`, distinct band).
    pub const COMPLIANCE_LAYOUT_INVALID: ProgramError = ProgramError::Custom(0x2010);
    pub const COMPLIANCE_STATUS_UNKNOWN: ProgramError = ProgramError::Custom(0x2011);
    pub const COMPLIANCE_UNVERIFIED: ProgramError = ProgramError::Custom(0x2012);
    pub const COMPLIANCE_SUSPENDED: ProgramError = ProgramError::Custom(0x2013);
    pub const COMPLIANCE_BLOCKED: ProgramError = ProgramError::Custom(0x2014);
    pub const RECORD_MISMATCH: ProgramError = ProgramError::Custom(0x2015);
}

/// On-chain swap ticket. `repr(C)` + `Pod`: the byte layout is the contract.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct SwapTicket {
    pub bump: u8,
    pub funded_a: u8,
    pub funded_b: u8,
    pub settled: u8,
    pub decimals_a: u8,
    pub decimals_b: u8,
    pub _pad: [u8; 2],
    pub party_a: [u8; 32],
    pub party_b: [u8; 32],
    pub mint_a: [u8; 32],
    pub mint_b: [u8; 32],
    pub amount_a: u64,
    pub amount_b: u64,
    pub expiry_slot: u64,
    pub nonce: u64,
}

impl SwapTicket {
    pub const LEN: usize = core::mem::size_of::<SwapTicket>();
}

// ─── Entrypoint dispatch ─────────────────────────────────────────────────

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let (tag, rest) = data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    match *tag {
        ix::CREATE_SWAP => create_swap(program_id, accounts, rest),
        ix::FUND => fund(program_id, accounts, rest),
        ix::SETTLE => settle(program_id, accounts, rest),
        ix::RECLAIM => reclaim(program_id, accounts, rest),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

// ─── create_swap ─────────────────────────────────────────────────────────
//
// Accounts:
//   [0, signer, write] payer
//   [1, write]         ticket PDA (uninitialized)
//   [2]                system_program
//
// Data: party_a(32) party_b(32) mint_a(32) mint_b(32)
//       amount_a(8) amount_b(8) decimals_a(1) decimals_b(1)
//       expiry_slot(8) nonce(8)   = 162 bytes
fn create_swap(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [payer_ai, ticket_ai, _system_ai] = match accounts {
        [p, t, s, ..] => [p, t, s],
        _ => return Err(ProgramError::NotEnoughAccountKeys),
    };
    require_signer(payer_ai)?;
    if data.len() < 162 {
        return Err(err::DATA_TOO_SHORT);
    }
    let party_a: [u8; 32] = data[0..32].try_into().unwrap();
    let party_b: [u8; 32] = data[32..64].try_into().unwrap();
    let mint_a: [u8; 32] = data[64..96].try_into().unwrap();
    let mint_b: [u8; 32] = data[96..128].try_into().unwrap();
    let amount_a = u64::from_le_bytes(data[128..136].try_into().unwrap());
    let amount_b = u64::from_le_bytes(data[136..144].try_into().unwrap());
    let decimals_a = data[144];
    let decimals_b = data[145];
    let expiry_slot = u64::from_le_bytes(data[146..154].try_into().unwrap());
    let nonce = u64::from_le_bytes(data[154..162].try_into().unwrap());

    let nonce_le = nonce.to_le_bytes();
    let (expected, bump) = find_program_address(
        &[DVP, &party_a, &party_b, &mint_a, &mint_b, &nonce_le],
        program_id,
    );
    if &expected != ticket_ai.key() {
        return Err(err::TICKET_PDA_MISMATCH);
    }

    let lamports = Rent::get()?.minimum_balance(SwapTicket::LEN);
    let bump_seed = [bump];
    let pda_seeds = [
        Seed::from(DVP),
        Seed::from(&party_a[..]),
        Seed::from(&party_b[..]),
        Seed::from(&mint_a[..]),
        Seed::from(&mint_b[..]),
        Seed::from(&nonce_le[..]),
        Seed::from(&bump_seed[..]),
    ];
    let pda_signer = Signer::from(&pda_seeds);
    CreateAccount {
        from: payer_ai,
        to: ticket_ai,
        lamports,
        space: SwapTicket::LEN as u64,
        owner: program_id,
    }
    .invoke_signed(&[pda_signer])?;

    let mut raw = ticket_ai.try_borrow_mut_data()?;
    let t: &mut SwapTicket = bytemuck::try_from_bytes_mut(&mut raw[..SwapTicket::LEN])
        .map_err(|_| err::TICKET_LAYOUT_INVALID)?;
    *t = SwapTicket {
        bump,
        funded_a: 0,
        funded_b: 0,
        settled: 0,
        decimals_a,
        decimals_b,
        _pad: [0; 2],
        party_a,
        party_b,
        mint_a,
        mint_b,
        amount_a,
        amount_b,
        expiry_slot,
        nonce,
    };
    msg!("psol-settlement: create_swap");
    Ok(())
}

// ─── fund ────────────────────────────────────────────────────────────────
//
// Accounts:
//   [0, signer] funder (party_a or party_b; authority on source ATA)
//   [1, write]  ticket PDA
//   [2, write]  funder's source ATA
//   [3, write]  ticket's escrow ATA for this leg's mint
//   [4]         mint (this leg)
//   [5]         token_program (Token-2022)
fn fund(_program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [funder_ai, ticket_ai, source_ai, escrow_ai, mint_ai, token_program_ai] = match accounts {
        [f, t, s, e, m, tp, ..] => [f, t, s, e, m, tp],
        _ => return Err(ProgramError::NotEnoughAccountKeys),
    };
    require_signer(funder_ai)?;
    if token_program_ai.key() != &TOKEN_2022_PROGRAM_ID {
        return Err(err::TOKEN_PROGRAM_MISMATCH);
    }

    let mut raw = ticket_ai.try_borrow_mut_data()?;
    let t: &mut SwapTicket = bytemuck::try_from_bytes_mut(&mut raw[..SwapTicket::LEN])
        .map_err(|_| err::TICKET_LAYOUT_INVALID)?;
    if t.settled != 0 {
        return Err(err::ALREADY_SETTLED);
    }
    let funder = funder_ai.key();
    let (amount, decimals) = if funder == &t.party_a {
        if t.funded_a != 0 {
            return Err(err::ALREADY_FUNDED);
        }
        if mint_ai.key() != &t.mint_a {
            return Err(err::MINT_MISMATCH);
        }
        t.funded_a = 1;
        (t.amount_a, t.decimals_a)
    } else if funder == &t.party_b {
        if t.funded_b != 0 {
            return Err(err::ALREADY_FUNDED);
        }
        if mint_ai.key() != &t.mint_b {
            return Err(err::MINT_MISMATCH);
        }
        t.funded_b = 1;
        (t.amount_b, t.decimals_b)
    } else {
        return Err(err::NOT_A_PARTY);
    };
    drop(raw);

    cpi_transfer_checked(
        token_program_ai.key(),
        source_ai,
        mint_ai,
        escrow_ai,
        funder_ai,
        amount,
        decimals,
        &[],
    )?;
    msg!("psol-settlement: fund");
    Ok(())
}

// ─── settle ──────────────────────────────────────────────────────────────
//
// Accounts:
//   [0, write] ticket PDA
//   [1, write] escrow ATA (mint_a)
//   [2, write] escrow ATA (mint_b)
//   [3, write] party_a's ATA (mint_b) — receives cash leg
//   [4, write] party_b's ATA (mint_a) — receives asset leg
//   [5]        mint_a
//   [6]        mint_b
//   [7]        party_a AccountRecord
//   [8]        party_b AccountRecord
//   [9]        compliance program (owner reference for the records)
//   [10]       token_program (Token-2022)
fn settle(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [ticket_ai, escrow_a_ai, escrow_b_ai, party_a_dst_ai, party_b_dst_ai, mint_a_ai, mint_b_ai, record_a_ai, record_b_ai, compliance_program_ai, token_program_ai] =
        match accounts {
            [t, ea, eb, ad, bd, ma, mb, ra, rb, cp, tp, ..] => {
                [t, ea, eb, ad, bd, ma, mb, ra, rb, cp, tp]
            }
            _ => return Err(ProgramError::NotEnoughAccountKeys),
        };
    if token_program_ai.key() != &TOKEN_2022_PROGRAM_ID {
        return Err(err::TOKEN_PROGRAM_MISMATCH);
    }

    let raw = ticket_ai.try_borrow_data()?;
    let t: SwapTicket = *bytemuck::try_from_bytes::<SwapTicket>(&raw[..SwapTicket::LEN])
        .map_err(|_| err::TICKET_LAYOUT_INVALID)?;
    drop(raw);

    if t.settled != 0 {
        return Err(err::ALREADY_SETTLED);
    }
    if t.funded_a == 0 || t.funded_b == 0 {
        return Err(err::NOT_FULLY_FUNDED);
    }
    if Clock::get()?.slot > t.expiry_slot {
        return Err(err::EXPIRED);
    }
    if mint_a_ai.key() != &t.mint_a || mint_b_ai.key() != &t.mint_b {
        return Err(err::MINT_MISMATCH);
    }

    // Both parties must currently pass compliance.
    verify_record(record_a_ai, compliance_program_ai.key(), &t.party_a)?;
    verify_record(record_b_ai, compliance_program_ai.key(), &t.party_b)?;

    // Verify the ticket PDA + build its signer.
    let nonce_le = t.nonce.to_le_bytes();
    let bump_seed = [t.bump];
    let pda_seeds = [
        Seed::from(DVP),
        Seed::from(&t.party_a[..]),
        Seed::from(&t.party_b[..]),
        Seed::from(&t.mint_a[..]),
        Seed::from(&t.mint_b[..]),
        Seed::from(&nonce_le[..]),
        Seed::from(&bump_seed[..]),
    ];
    let expected = pinocchio::pubkey::create_program_address(
        &[DVP, &t.party_a, &t.party_b, &t.mint_a, &t.mint_b, &nonce_le, &bump_seed],
        program_id,
    )?;
    if &expected != ticket_ai.key() {
        return Err(err::TICKET_PDA_MISMATCH);
    }
    let signer = Signer::from(&pda_seeds);

    // Deliver asset leg (mint_a) to party_b, cash leg (mint_b) to party_a.
    cpi_transfer_checked(
        token_program_ai.key(),
        escrow_a_ai,
        mint_a_ai,
        party_b_dst_ai,
        ticket_ai,
        t.amount_a,
        t.decimals_a,
        &[signer.clone()],
    )?;
    cpi_transfer_checked(
        token_program_ai.key(),
        escrow_b_ai,
        mint_b_ai,
        party_a_dst_ai,
        ticket_ai,
        t.amount_b,
        t.decimals_b,
        &[signer.clone()],
    )?;

    let mut raw = ticket_ai.try_borrow_mut_data()?;
    let t2: &mut SwapTicket = bytemuck::try_from_bytes_mut(&mut raw[..SwapTicket::LEN])
        .map_err(|_| err::TICKET_LAYOUT_INVALID)?;
    t2.settled = 1;
    msg!("psol-settlement: settle");
    Ok(())
}

// ─── reclaim ─────────────────────────────────────────────────────────────
//
// After expiry, refund each funded leg to its owner. Accounts:
//   [0, write] ticket PDA
//   [1, write] escrow ATA (mint_a)
//   [2, write] escrow ATA (mint_b)
//   [3, write] party_a's ATA (mint_a) — refund asset leg
//   [4, write] party_b's ATA (mint_b) — refund cash leg
//   [5]        mint_a
//   [6]        mint_b
//   [7]        token_program (Token-2022)
fn reclaim(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [ticket_ai, escrow_a_ai, escrow_b_ai, party_a_dst_ai, party_b_dst_ai, mint_a_ai, mint_b_ai, token_program_ai] =
        match accounts {
            [t, ea, eb, ad, bd, ma, mb, tp, ..] => [t, ea, eb, ad, bd, ma, mb, tp],
            _ => return Err(ProgramError::NotEnoughAccountKeys),
        };
    if token_program_ai.key() != &TOKEN_2022_PROGRAM_ID {
        return Err(err::TOKEN_PROGRAM_MISMATCH);
    }

    let raw = ticket_ai.try_borrow_data()?;
    let t: SwapTicket = *bytemuck::try_from_bytes::<SwapTicket>(&raw[..SwapTicket::LEN])
        .map_err(|_| err::TICKET_LAYOUT_INVALID)?;
    drop(raw);

    if t.settled != 0 {
        return Err(err::ALREADY_SETTLED);
    }
    if Clock::get()?.slot <= t.expiry_slot {
        return Err(err::NOT_EXPIRED);
    }
    if t.funded_a == 0 && t.funded_b == 0 {
        return Err(err::NOTHING_TO_RECLAIM);
    }
    if mint_a_ai.key() != &t.mint_a || mint_b_ai.key() != &t.mint_b {
        return Err(err::MINT_MISMATCH);
    }

    let nonce_le = t.nonce.to_le_bytes();
    let bump_seed = [t.bump];
    let pda_seeds = [
        Seed::from(DVP),
        Seed::from(&t.party_a[..]),
        Seed::from(&t.party_b[..]),
        Seed::from(&t.mint_a[..]),
        Seed::from(&t.mint_b[..]),
        Seed::from(&nonce_le[..]),
        Seed::from(&bump_seed[..]),
    ];
    let expected = pinocchio::pubkey::create_program_address(
        &[DVP, &t.party_a, &t.party_b, &t.mint_a, &t.mint_b, &nonce_le, &bump_seed],
        program_id,
    )?;
    if &expected != ticket_ai.key() {
        return Err(err::TICKET_PDA_MISMATCH);
    }
    let signer = Signer::from(&pda_seeds);

    if t.funded_a != 0 {
        cpi_transfer_checked(
            token_program_ai.key(),
            escrow_a_ai,
            mint_a_ai,
            party_a_dst_ai,
            ticket_ai,
            t.amount_a,
            t.decimals_a,
            &[signer.clone()],
        )?;
    }
    if t.funded_b != 0 {
        cpi_transfer_checked(
            token_program_ai.key(),
            escrow_b_ai,
            mint_b_ai,
            party_b_dst_ai,
            ticket_ai,
            t.amount_b,
            t.decimals_b,
            &[signer.clone()],
        )?;
    }

    let mut raw = ticket_ai.try_borrow_mut_data()?;
    let t2: &mut SwapTicket = bytemuck::try_from_bytes_mut(&mut raw[..SwapTicket::LEN])
        .map_err(|_| err::TICKET_LAYOUT_INVALID)?;
    t2.settled = 1;
    msg!("psol-settlement: reclaim");
    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────

fn require_signer(ai: &AccountInfo) -> ProgramResult {
    if !ai.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    Ok(())
}

enum CheckErrorOrMismatch {
    Check(CheckError),
    Mismatch,
}

fn verify_record(
    record_ai: &AccountInfo,
    compliance_program: &Pubkey,
    expected_participant: &[u8; 32],
) -> ProgramResult {
    let res = (|| -> Result<(), CheckErrorOrMismatch> {
        if record_ai.owner() != compliance_program {
            return Err(CheckErrorOrMismatch::Mismatch);
        }
        let data = record_ai
            .try_borrow_data()
            .map_err(|_| CheckErrorOrMismatch::Mismatch)?;
        let record = read_account_record(&data).map_err(CheckErrorOrMismatch::Check)?;
        if &record.participant != expected_participant {
            return Err(CheckErrorOrMismatch::Mismatch);
        }
        record
            .check_transfer_allowed()
            .map_err(CheckErrorOrMismatch::Check)
    })();
    res.map_err(|e| match e {
        CheckErrorOrMismatch::Check(c) => match c {
            CheckError::LayoutInvalid => err::COMPLIANCE_LAYOUT_INVALID,
            CheckError::StatusUnknown => err::COMPLIANCE_STATUS_UNKNOWN,
            CheckError::Unverified => err::COMPLIANCE_UNVERIFIED,
            CheckError::Suspended => err::COMPLIANCE_SUSPENDED,
            CheckError::Blocked => err::COMPLIANCE_BLOCKED,
        },
        CheckErrorOrMismatch::Mismatch => err::RECORD_MISMATCH,
    })
}

/// Construct + invoke a Token-2022 `TransferChecked` CPI.
#[allow(clippy::too_many_arguments)]
fn cpi_transfer_checked(
    token_program_id: &Pubkey,
    source: &AccountInfo,
    mint: &AccountInfo,
    destination: &AccountInfo,
    authority: &AccountInfo,
    amount: u64,
    decimals: u8,
    signers: &[Signer<'_, '_>],
) -> ProgramResult {
    let mut data = [0u8; TRANSFER_CHECKED_DATA_LEN];
    data[0] = TOKEN_IX_TRANSFER_CHECKED;
    data[1..9].copy_from_slice(&amount.to_le_bytes());
    data[9] = decimals;

    let metas = [
        AccountMeta::new(source.key(), true, false),
        AccountMeta::new(mint.key(), false, false),
        AccountMeta::new(destination.key(), true, false),
        AccountMeta::new(authority.key(), false, true),
    ];
    let ix = Instruction {
        program_id: token_program_id,
        data: &data,
        accounts: &metas,
    };
    let infos = [source, mint, destination, authority];
    pinocchio::cpi::slice_invoke_signed(&ix, &infos, signers)
}
