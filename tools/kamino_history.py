import os
import sys
import requests
import json
from datetime import datetime, timedelta
from dotenv import load_dotenv

# --- Configuration ---
load_dotenv()
RPC_URL = os.getenv("RPC_URL")
KAMINO_PROGRAM_ID = "KLend2VCL2syzzZbsiByMvKm9teD9NQUsc9X744rMvR"

def fetch_signatures(limit=100):
    """Fetch recent transaction signatures for the Kamino program."""
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSignaturesForAddress",
        "params": [
            KAMINO_PROGRAM_ID,
            {"limit": limit}
        ]
    }
    response = requests.post(RPC_URL, json=payload)
    return response.json().get("result", [])

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
    response = requests.post(RPC_URL, json=payload)
    return response.json().get("result", {})

def is_liquidation(tx_data):
    """Check if the transaction contains a LiquidateObligation instruction."""
    if not tx_data or "meta" not in tx_data or "logMessages" not in tx_data["meta"]:
        return False
    
    logs = tx_data["meta"]["logMessages"]
    return any("Instruction: LiquidateObligation" in log for log in logs)

def main():
    if not RPC_URL:
        print("Error: RPC_URL not found in .env file.")
        sys.exit(1)

    print(f"# Historical Kamino Liquidations (Last {KAMINO_PROGRAM_ID[:8]}... signatures)")
    print(f"Generated at: {datetime.now().isoformat()}\n")
    
    print("| Timestamp | Signature | Status | Logs Preview |")
    print("| :--- | :--- | :--- | :--- |")

    signatures = fetch_signatures(50)  # Scan last 50 for demo
    found_any = False

    for sig_info in signatures:
        sig = sig_info["signature"]
        block_time = sig_info.get("blockTime")
        dt = datetime.fromtimestamp(block_time).strftime('%Y-%m-%d %H:%M:%S') if block_time else "N/A"
        
        tx_details = fetch_transaction_details(sig)
        
        if is_liquidation(tx_details):
            found_any = True
            status = "✅ SUCCESS" if tx_details["meta"].get("err") is None else "❌ FAILED"
            # Extract a bit of context from logs
            logs = tx_details["meta"]["logMessages"]
            preview = next((l for l in logs if "repay_amount" in l or "withdrawn_amount" in l), "No amount logs found")
            print(f"| {dt} | `{sig}` | {status} | `{preview}` |")

    if not found_any:
        print("\n*No liquidations found in the scanned range.*")

if __name__ == "__main__":
    main()
