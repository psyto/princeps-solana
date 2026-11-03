//! Decide from an `AccountRecord`'s raw bytes whether its wallet may
//! transfer — the same check every psol program makes. Use it off-chain on
//! bytes fetched over RPC, or on-chain on an `AccountInfo`'s data.
//!
//!     cargo run -p psol-types --example check_record

use psol_types::{AccountRecord, compliance_status, jurisdiction, read_account_record};

fn main() {
    let mut record = AccountRecord::pending([1; 32], jurisdiction::JP, 0, 255);
    for status in [compliance_status::PENDING, compliance_status::VERIFIED, compliance_status::SUSPENDED] {
        record.status = status;
        let bytes = bytemuck::bytes_of(&record); // what the record PDA holds
        let check = read_account_record(bytes).and_then(|r| r.check_transfer_allowed());
        println!("status {status}  ->  {check:?}");
    }
}
