import os
import sys
import requests
import json
import time
import argparse
from datetime import datetime
from dotenv import load_dotenv

# --- Configuration ---
load_dotenv()
RPC_URL = os.getenv("RPC_URL")
KAMINO_PROGRAM_ID = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD"
BATCH_SIZE = 1000
MAX_BATCHES_LIMIT = 20
SLEEP_ON_RATE_LIMIT = 2.0  # seconds to wait if rate limited

MAX_RETRIES = 10

def fetch_signatures(before=None, limit=1000):
    """Fetch recent transaction signatures for the Kamino program."""
    params = {"limit": limit}
    if before:
        params["before"] = before

    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSignaturesForAddress",
        "params": [
            KAMINO_PROGRAM_ID,
            params
        ]
    }
    for attempt in range(1, MAX_RETRIES + 1):
        try:
            response = requests.post(RPC_URL, json=payload, timeout=60)
            if response.status_code == 429:
                print(f"\nRate limit hit (429) on getSignaturesForAddress, sleeping {SLEEP_ON_RATE_LIMIT}s... (attempt {attempt}/{MAX_RETRIES})")
                time.sleep(SLEEP_ON_RATE_LIMIT)
                continue
            response.raise_for_status()
            res_json = response.json()
            if "error" in res_json:
                print(f"\nRPC Error: {res_json['error']}")
                return []
            return res_json.get("result", [])
        except Exception as e:
            print(f"\nError fetching signatures (attempt {attempt}/{MAX_RETRIES}): {e}")
            if attempt < MAX_RETRIES:
                time.sleep(SLEEP_ON_RATE_LIMIT)
    print(f"\nFailed to fetch signatures after {MAX_RETRIES} attempts. Aborting.")
    return []

def fetch_transaction_details(signature):
    """Fetch full transaction details including logs."""
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [
            signature,
            {"encoding": "json", "maxSupportedTransactionVersion": 0}
        ]
    }
    for attempt in range(1, MAX_RETRIES + 1):
        try:
            response = requests.post(RPC_URL, json=payload, timeout=60)
            if response.status_code == 429:
                print(f"\nRate limit hit (429) on getTransaction, sleeping {SLEEP_ON_RATE_LIMIT}s... (attempt {attempt}/{MAX_RETRIES})")
                time.sleep(SLEEP_ON_RATE_LIMIT)
                continue
            response.raise_for_status()
            res_json = response.json()
            if "error" in res_json:
                # Some errors like 'not found' can happen for very recent tx — not retriable
                return {}
            return res_json.get("result") or {}
        except Exception as e:
            print(f"\nError fetching transaction {signature[:8]}... (attempt {attempt}/{MAX_RETRIES}): {e}")
            if attempt < MAX_RETRIES:
                time.sleep(SLEEP_ON_RATE_LIMIT)
    print(f"\nFailed to fetch transaction {signature[:8]}... after {MAX_RETRIES} attempts. This transaction will be skipped — potential false negative.")
    return None

def is_liquidation(tx_data):
    """Check if the transaction contains a successful liquidation instruction.
    Returns None if tx_data is None (fetch failed), False if not a liquidation, True if it is."""
    if tx_data is None:
        return None  # fetch failed — caller must handle this as a potential false negative
    if not tx_data or "meta" not in tx_data or tx_data["meta"] is None:
        return False

    # Exclude failed transactions — no state change occurred on-chain
    if tx_data["meta"].get("err") is not None:
        return False

    logs = tx_data["meta"].get("logMessages")
    if not logs:
        return False
    return any("Instruction: LiquidateObligationAndRedeemReserveCollateral" in log for log in logs)

def main():
    parser = argparse.ArgumentParser(description="Scan Kamino liquidation history.")
    parser.add_argument("--days", type=int, default=1, help="Number of days to scan (default: 1)")
    args = parser.parse_args()

    if not RPC_URL:
        print("Error: RPC_URL not found in .env file.")
        sys.exit(1)

    cutoff_timestamp = time.time() - (args.days * 24 * 3600)
    cutoff_dt = datetime.fromtimestamp(cutoff_timestamp).strftime('%Y-%m-%d %H:%M:%S')

    print(f"Scanning Kamino history ({KAMINO_PROGRAM_ID[:8]}...) for liquidations...")
    print(f"Time range: Last {args.days} day(s) (since {cutoff_dt})")
    print(f"Hard limit: {MAX_BATCHES_LIMIT} batches of {BATCH_SIZE} transactions.")
    
    found_liquidations = []
    skipped_fetch_errors = []
    last_signature = None
    batch_num = 0
    total_checked = 0
    reached_cutoff = False
    hit_batch_limit = False
    
    start_time = time.time()

    try:
        while batch_num < MAX_BATCHES_LIMIT and not reached_cutoff:
            batch_num += 1
            print(f"\nScanning batch {batch_num}/{MAX_BATCHES_LIMIT} (before: {last_signature or 'latest'})...")
            
            signatures = fetch_signatures(before=last_signature, limit=BATCH_SIZE)
            if not signatures:
                print("No more signatures found.")
                break
                
            for sig_info in signatures:
                sig = sig_info["signature"]
                last_signature = sig

                block_time = sig_info.get("blockTime")
                if block_time is not None and block_time < cutoff_timestamp:
                    reached_cutoff = True
                    break

                total_checked += 1
                
                tx_details = fetch_transaction_details(sig)
                
                result = is_liquidation(tx_details)
                if result is None:
                    skipped_fetch_errors.append(sig)
                elif result is True:
                    dt = datetime.fromtimestamp(block_time).strftime('%Y-%m-%d %H:%M:%S') if block_time else "N/A"
                    logs = tx_details["meta"].get("logMessages", [])
                    preview = next((l for l in logs if "repay_amount" in l or "withdrawn_amount" in l), "No amount logs found")
                    liquidation_info = {
                        "timestamp": dt,
                        "signature": sig,
                        "status": "✅ SUCCESS",
                        "preview": preview
                    }
                    found_liquidations.append(liquidation_info)
                    print(f"\n  [FOUND {len(found_liquidations)}] {dt} | {sig}")
                
                # Progress indicator
                if total_checked % 50 == 0:
                    sys.stdout.write(f"\r  Processed {total_checked} transactions...")
                    sys.stdout.flush()

    except KeyboardInterrupt:
        print("\nScan interrupted by user.")

    hit_batch_limit = (batch_num >= MAX_BATCHES_LIMIT and not reached_cutoff)

    total_time = time.time() - start_time
    print(f"\n\nScan completed in {total_time:.1f}s.")
    print(f"Total transactions checked in the last {args.days} day(s): {total_checked}")
    print(f"Total liquidations found: {len(found_liquidations)}")

    if hit_batch_limit:
        print(f"\n⚠️  WARNING: Batch limit ({MAX_BATCHES_LIMIT}) reached before covering the full {args.days}-day period. Results may be INCOMPLETE.")
    if skipped_fetch_errors:
        print(f"\n⚠️  WARNING: {len(skipped_fetch_errors)} transaction(s) could not be fetched after {MAX_RETRIES} retries and were skipped — potential false negatives.")

    if not found_liquidations:
        print(f"No liquidations occurred on Kamino in the last {args.days} days.")

    # Generate Markdown Report
    lines = []
    lines.append(f"# Historical Kamino Liquidations Report")
    lines.append(f"- Generated at: {datetime.now().isoformat()}")
    lines.append(f"- Time period: Last {args.days} day(s) (Since {cutoff_dt})")
    lines.append(f"- Program ID: `{KAMINO_PROGRAM_ID}`")
    lines.append(f"- Batches scanned: {batch_num}")
    lines.append(f"- Transactions checked: {total_checked}")
    lines.append(f"- Liquidations found: {len(found_liquidations)}")
    if skipped_fetch_errors:
        lines.append(f"- ⚠️ Transactions skipped due to fetch errors: {len(skipped_fetch_errors)} (potential false negatives)")
    if hit_batch_limit:
        lines.append(f"- ⚠️ **INCOMPLETE SCAN**: batch limit ({MAX_BATCHES_LIMIT}) reached before covering the full period. Increase MAX_BATCHES_LIMIT.")
    lines.append("")

    if not found_liquidations:
        lines.append(f"**No liquidations occurred on Kamino in the last {args.days} days.**")
    else:
        lines.append("| Timestamp | Signature | Status | Logs Preview |")
        lines.append("| :--- | :--- | :--- | :--- |")
        for liq in found_liquidations:
            lines.append(f"| {liq['timestamp']} | `{liq['signature']}` | {liq['status']} | `{liq['preview']}` |")

    report_content = "\n".join(lines)
    
    report_path = "tools/last_report.md"
    try:
        with open(report_path, "w", encoding="utf-8") as f:
            f.write(report_content)
        print(f"Report saved to {report_path}")
    except Exception as e:
        print(f"Error saving report to {report_path}: {e}")

if __name__ == "__main__":
    main()
