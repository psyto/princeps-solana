//! Test-only program: accepts any instruction and does nothing. The e2e
//! tests deploy it as a caller-supplied "vault program" to check that repo
//! and lending refuse CPIs into a program other than the one that owns the
//! recorded vaults.

#![no_std]

use pinocchio::{ProgramResult, account_info::AccountInfo, pubkey::Pubkey};

pinocchio::program_entrypoint!(process_instruction);
pinocchio::default_allocator!();
pinocchio::nostd_panic_handler!();

pub fn process_instruction(_: &Pubkey, _: &[AccountInfo], _: &[u8]) -> ProgramResult {
    Ok(())
}
