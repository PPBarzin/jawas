pub const KAMINO_PROGRAM_ID: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
pub const SOLEND_PROGRAM_ID: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Protocol {
    Kamino,
    Solend,
}

impl Protocol {
    pub fn program_id(&self) -> &'static str {
        match self {
            Protocol::Kamino => KAMINO_PROGRAM_ID,
            Protocol::Solend => SOLEND_PROGRAM_ID,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Protocol::Kamino => "Kamino",
            Protocol::Solend => "Solend",
        }
    }
}
