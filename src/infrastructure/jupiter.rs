use crate::ports::jupiter::{JupiterPort, QuoteResponse};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use solana_sdk::instruction::{Instruction, AccountMeta};
use solana_sdk::pubkey::Pubkey;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use base64::{Engine as _, engine::general_purpose};

pub struct JupiterAdapter {
    client: Client,
    base_url: String,
}

impl JupiterAdapter {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.unwrap_or("https://quote-api.jup.ag/v6").to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct JupiterQuoteResponse {
    input_mint: String,
    in_amount: String,
    output_mint: String,
    out_amount: String,
    price_impact_pct: String,
    // Other fields are kept in a Value to be passed back to swap-instructions if needed
    #[serde(flatten)]
    extra: serde_json::Value,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SwapInstructionsResponse {
    setup_instructions: Vec<JupiterInstruction>,
    swap_instruction: JupiterInstruction,
    cleanup_instruction: Option<JupiterInstruction>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct JupiterInstruction {
    program_id: String,
    accounts: Vec<JupiterAccountMeta>,
    data: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct JupiterAccountMeta {
    pubkey: String,
    is_signer: bool,
    is_writable: bool,
}

impl TryFrom<JupiterInstruction> for Instruction {
    type Error = anyhow::Error;

    fn try_from(val: JupiterInstruction) -> Result<Self, Self::Error> {
        let program_id = Pubkey::from_str(&val.program_id)
            .map_err(|e| anyhow::anyhow!("Invalid program ID: {}", e))?;
        let accounts = val.accounts.into_iter().map(|a| {
            Ok(AccountMeta {
                pubkey: Pubkey::from_str(&a.pubkey)
                    .map_err(|e| anyhow::anyhow!("Invalid pubkey: {}", e))?,
                is_signer: a.is_signer,
                is_writable: a.is_writable,
            })
        }).collect::<Result<Vec<_>>>()?;
        let data = general_purpose::STANDARD.decode(&val.data)
            .map_err(|e| anyhow::anyhow!("Failed to decode base64 data: {}", e))?;
        Ok(Instruction {
            program_id,
            accounts,
            data,
        })
    }
}

#[async_trait]
impl JupiterPort for JupiterAdapter {
    async fn get_quote(
        &self,
        input_mint: &str,
        output_mint: &str,
        amount: u64,
        slippage_bps: u16,
    ) -> Result<QuoteResponse> {
        let url = format!("{}/quote", self.base_url);
        let response = self.client.get(&url)
            .query(&[
                ("inputMint", input_mint),
                ("outputMint", output_mint),
                ("amount", &amount.to_string()),
                ("slippageBps", &slippage_bps.to_string()),
            ])
            .send()
            .await?
            .json::<JupiterQuoteResponse>()
            .await?;

        Ok(QuoteResponse {
            in_amount: response.in_amount.parse()?,
            out_amount: response.out_amount.parse()?,
            price_impact_pct: response.price_impact_pct.parse()?,
            swap_transaction: None, // Only available via /swap endpoint, not /quote
        })
    }

    async fn build_swap_instructions(
        &self,
        input_mint: &str,
        output_mint: &str,
        amount: u64,
        slippage_bps: u16,
        user_public_key: &str,
    ) -> Result<Vec<Instruction>> {
        // First get the quote because we need the full quote response to call swap-instructions
        let url_quote = format!("{}/quote", self.base_url);
        let quote = self.client.get(&url_quote)
            .query(&[
                ("inputMint", input_mint),
                ("outputMint", output_mint),
                ("amount", &amount.to_string()),
                ("slippageBps", &slippage_bps.to_string()),
            ])
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        let url_swap = format!("{}/swap-instructions", self.base_url);
        let body = serde_json::json!({
            "quoteResponse": quote,
            "userPublicKey": user_public_key,
            "wrapAndUnwrapSol": true,
        });

        let response = self.client.post(&url_swap)
            .json(&body)
            .send()
            .await?
            .json::<SwapInstructionsResponse>()
            .await?;

        let mut instructions = Vec::new();
        for j_ix in response.setup_instructions {
            instructions.push(Instruction::try_from(j_ix)?);
        }
        instructions.push(Instruction::try_from(response.swap_instruction)?);
        if let Some(j_ix) = response.cleanup_instruction {
            instructions.push(Instruction::try_from(j_ix)?);
        }

        Ok(instructions)
    }
}
