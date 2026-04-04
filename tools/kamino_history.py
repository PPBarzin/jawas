import os
import sys
import requests
import json
import time
from datetime import datetime
from dotenv import load_dotenv

# --- Configuration ---
load_dotenv()
RPC_URL = os.getenv("RPC_URL")
KAMINO_PROGRAM_ID = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD"
BATCH_SIZE = 1000
MAX_BATCHES = 5
MIN_LIQUIDATIONS = 3
SLEEP_ON_RATE_LIMIT = 2.0  # seconds to wait if rate limited

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
    try:
        response = requests.post(RPC_URL, json=payload, timeout=60)
        if response.status_code == 429:
            print(f"\nRate limit hit (429) on getSignaturesForAddress, sleeping {SLEEP_ON_RATE_LIMIT}s...")
            time.sleep(SLEEP_ON_RATE_LIMIT)
            return fetch_signatures(before, limit)
        response.raise_for_status()
        res_json = response.json()
        if "error" in res_json:
            print(f"\nRPC Error: {res_json['error']}")
            return []
        return res_json.get("result", [])
    except Exception as e:
        print(f"\nError fetching signatures: {e}")
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
    try:
        response = requests.post(RPC_URL, json=payload, timeout=60)
        if response.status_code == 429:
            time.sleep(SLEEP_ON_RATE_LIMIT)
            return fetch_transaction_details(signature)
        response.raise_for_status()
        res_json = response.json()
        if "error" in res_json:
            # Some errors like 'not found' can happen for very recent tx
            return {}
        return res_json.get("result", {})
    except Exception as e:
        return {}

def is_liquidation(tx_data):
    """Check if the transaction contains a LiquidateObligation instruction."""
    if not tx_data or "meta" not in tx_data or tx_data["meta"] is None:
        return False
    
    logs = tx_data["meta"].get("logMessages")
    if not logs:
        return False
    return any("Instruction: LiquidateObligation" in log for log in logs)

def main():
    if not RPC_URL:
        print("Error: RPC_URL not found in .env file.")
        sys.exit(1)

    print(f"Scanning Kamino history ({KAMINO_PROGRAM_ID[:8]}...) for liquidations...")
    print(f"Goal: Find {MIN_LIQUIDATIONS} liquidations or scan up to {MAX_BATCHES} batches of {BATCH_SIZE} transactions.")
    
    found_liquidations = []
    last_signature = None
    batch_num = 0
    total_checked = 0
    
    start_time = time.time()

    try:
        while batch_num < MAX_BATCHES and len(found_liquidations) < MIN_LIQUIDATIONS:
            batch_num += 1
            print(f"\nScanning batch {batch_num}/{MAX_BATCHES} (before: {last_signature or 'latest'})...")
            
            signatures = fetch_signatures(before=last_signature, limit=BATCH_SIZE)
            if not signatures:
                print("No more signatures found.")
                break
                
            for i, sig_info in enumerate(signatures):
                sig = sig_info["signature"]
                last_signature = sig 
                total_checked += 1
                
                tx_details = fetch_transaction_details(sig)
                
                if is_liquidation(tx_details):
                    block_time = sig_info.get("blockTime")
                    dt = datetime.fromtimestamp(block_time).strftime('%Y-%m-%d %H:%M:%S') if block_time else "N/A"
                    
                    err = tx_details["meta"].get("err")
                    status = "✅ SUCCESS" if err is None else "❌ FAILED"
                    
                    logs = tx_details["meta"].get("logMessages", [])
                    preview = next((l for l in logs if "repay_amount" in l or "withdrawn_amount" in l), "No amount logs found")
                    
                    liquidation_info = {
                        "timestamp": dt,
                        "signature": sig,
                        "status": status,
                        "preview": preview
                    }
                    found_liquidations.append(liquidation_info)
                    print(f"\n  [FOUND {len(found_liquidations)}] {dt} | {sig} | {status}")
                    
                    if len(found_liquidations) >= MIN_LIQUIDATIONS:
                        break
                
                # Progress indicator
                if total_checked % 50 == 0:
                    sys.stdout.write(f"\r  Processed {total_checked} transactions...")
                    sys.stdout.flush()

            if len(found_liquidations) >= MIN_LIQUIDATIONS:
                break
    except KeyboardInterrupt:
        print("\nScan interrupted by user.")

    total_time = time.time() - start_time
    print(f"\n\nScan completed in {total_time:.1f}s.")
    print(f"Total transactions checked: {total_checked}")
    print(f"Total liquidations found: {len(found_liquidations)}")

    # Generate Markdown Report
    lines = []
    lines.append(f"# Historical Kamino Liquidations Report")
    lines.append(f"- Generated at: {datetime.now().isoformat()}")
    lines.append(f"- Program ID: `{KAMINO_PROGRAM_ID}`")
    lines.append(f"- Batches scanned: {batch_num}")
    lines.append(f"- Transactions checked: {total_checked}")
    lines.append(f"- Liquidations found: {len(found_liquidations)}\n")
    
    lines.append("| Timestamp | Signature | Status | Logs Preview |")
    lines.append("| :--- | :--- | :--- | :--- |")

    if not found_liquidations:
        lines.append("| N/A | N/A | N/A | No liquidations found in the scanned range. |")
    else:
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
