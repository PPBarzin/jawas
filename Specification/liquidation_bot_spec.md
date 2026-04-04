# Jawas — Liquidation Bot

> *"UTINI !!"* — cri de victoire à chaque liquidation réussie
>
> **Statut** : Concept / Recherche  
> **Auteur** : Pierre-Philippe Barzin  
> **Date** : Avril 2026  
> **Protocole cible** : Kamino Finance (Solana)

---

## Pourquoi Jawas ?

Les Jawas parcourent le désert de Tatooine pour récupérer ce que les autres ont abandonné ou perdu. Ils ne créent pas — ils **récupèrent**.

C'est exactement ce que fait ce bot : il surveille les positions DeFi dégradées et récupère le collatéral que les emprunteurs ne peuvent plus défendre.

> *Les Jawas ne jugent pas. Ils ramassent.*

---

## 1. Contexte et objectif

### Qu'est-ce qu'une liquidation ?

Sur un protocole de lending comme Kamino, chaque emprunteur dépose du **collatéral** (ex. SOL) pour emprunter des stablecoins (ex. USDC). Tant que la valeur du collatéral reste suffisamment élevée par rapport à la dette, la position est saine.

Quand le prix du collatéral chute, le ratio **LTV (Loan-to-Value)** monte. Si le LTV dépasse un seuil critique (défini par le protocole), la position devient **liquidable** : n'importe qui peut rembourser une partie de la dette de l'emprunteur et recevoir en échange son collatéral avec une **décote de 5–10%** (liquidation bonus).

### Objectif du projet

Construire un bot autonome capable de :
1. Surveiller en temps réel toutes les positions ouvertes sur Kamino
2. Identifier les positions devenant liquidables
3. Calculer si la liquidation est rentable après frais
4. Exécuter la liquidation et sécuriser le profit

---

## 2. Mécanique détaillée d'une liquidation

### Exemple concret

```
Position d'un emprunteur :
  Collatéral  : 10 SOL @ $180 = $1,800
  Dette       : $1,170 USDC
  LTV actuel  : 65%  ←  dépasse le seuil Kamino (ex. 63%)

Intervention du liquidateur :
  1. Rembourse 50% de la dette : $585 USDC
  2. Reçoit du collatéral avec 8% de bonus :
       $585 / $180 = 3.25 SOL × 1.08 = 3.51 SOL
  3. Revend immédiatement 3.51 SOL → $631.8 USDC
  4. Profit brut = $631.8 - $585 = $46.8
```

### Règles importantes à connaître

- **Close factor** : sur la plupart des protocoles, on ne peut liquider qu'une fraction de la dette par appel (ex. 50%). Si la position est très dégradée, plusieurs liquidations successives sont nécessaires.
- **Liquidation threshold vs liquidation penalty** : deux paramètres distincts par token sur Kamino. Ils varient selon l'actif (SOL, ETH, BTC, stablecoins...).
- **Dust positions** : les très petites positions ($10–$50) sont rarement rentables une fois les frais de transaction inclus.

---

## 3. Architecture du bot

```
┌─────────────────────────────────────────────────────────────────┐
│                        LIQUIDATION BOT                           │
│                                                                   │
│   ┌─────────────┐    ┌──────────────┐    ┌───────────────────┐  │
│   │   MONITOR   │───▶│   ENGINE     │───▶│    EXECUTOR       │  │
│   │             │    │              │    │                   │  │
│   │ - Indexe    │    │ - Calcule    │    │ - Signe la tx     │  │
│   │   toutes    │    │   LTV temps  │    │ - Envoie via Jito │  │
│   │   positions │    │   réel       │    │ - Gère le swap    │  │
│   │ - Websocket │    │ - Score de   │    │   post-liquidation│  │
│   │   RPC       │    │   rentabilité│    │                   │  │
│   │ - Prix      │    │ - Priorise   │    └───────────────────┘  │
│   │   on-chain  │    │   la queue   │                           │
│   └─────────────┘    └──────────────┘                           │
│                                                                   │
│   ┌─────────────────────────────────────────────────────────┐   │
│   │                    CAPITAL MANAGER                       │   │
│   │  Capital propre  ←→  Flash Loan  ←→  Gestion du risque  │   │
│   └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

### 3.1 Module Monitor

**Rôle** : savoir en permanence quelles positions sont proches du seuil de liquidation.

**Sources de données :**
- **Websocket RPC Solana** : écoute les changements de compte on-chain en temps réel
- **API Kamino** : endpoint public listant les positions ouvertes et leurs paramètres
- **Prix oracle** : Kamino utilise Pyth Network pour les prix — s'abonner aux mises à jour de prix Pyth directement

**Ce qu'on indexe par position :**
```
{
  wallet_address,
  collateral_token,
  collateral_amount,
  debt_token,
  debt_amount,
  liquidation_threshold,   ← fourni par Kamino par token
  current_ltv,             ← calculé en temps réel
  distance_to_liquidation  ← (liquidation_threshold - current_ltv)
}
```

**Stratégie de surveillance :**
- Positions avec `distance_to_liquidation < 5%` → surveillance haute fréquence (chaque bloc ~400ms)
- Positions avec `distance_to_liquidation 5–15%` → surveillance basse fréquence (toutes les 30s)
- Positions au-delà → ignorées jusqu'au prochain grand mouvement de prix

### 3.2 Module Engine (Décision)

**Rôle** : décider si une liquidation est rentable et avec quels paramètres.

**Calcul de rentabilité :**

```
profit_brut = collateral_reçu_USD - dette_remboursée_USD
            = (debt_to_repay / collateral_price) × (1 + liquidation_bonus) × collateral_price
            - debt_to_repay
            = debt_to_repay × liquidation_bonus

coûts :
  - frais de transaction Solana    : ~0.000005 SOL (~négligeable)
  - tip Jito (priorité MEV)        : variable, 0.001–0.1 SOL selon compétition
  - slippage du swap post-liquidation : 0.1–1% selon la liquidité

condition d'exécution :
  profit_brut - tip_jito - frais_swap > seuil_minimum (ex. $20)
```

**Paramètres de décision à définir :**
- `min_profit_usd` : profit minimum pour déclencher (filtre les dust positions)
- `max_tip_jito_sol` : budget maximum de tip MEV
- `max_slippage_pct` : slippage toléré sur le swap post-liquidation

### 3.3 Module Executor

**Rôle** : construire, signer et envoyer la transaction de liquidation aussi vite que possible.

**Séquence d'une transaction :**

```
1. Construire l'instruction de liquidation Kamino
   └─ Paramètres : borrower_address, debt_amount, collateral_token

2. Construire l'instruction de swap (Jupiter)
   └─ Swapper le collatéral reçu → USDC pour sécuriser le profit

3. (Optionnel) Wrapper dans un Flash Loan si capital insuffisant

4. Signer avec le keypair du bot

5. Envoyer via Jito Bundle
   └─ Atomique : liquidation + swap dans le même bloc
   └─ Inclure le tip Jito calculé par l'Engine
```

**Gestion des échecs :**
- Si la transaction échoue (quelqu'un d'autre a liquidé avant) → log et passer à la suivante
- Si le slippage du swap dépasse le max → annuler ou ajuster le tip

### 3.4 Capital Manager

**Option A — Capital propre**
- Avantage : pas de frais de flash loan, transaction plus simple et plus rapide
- Inconvénient : capital immobilisé, limité par le solde disponible
- Recommandé pour démarrer : $10,000–$50,000 USDC sur le wallet du bot

**Option B — Flash Loan**
- Avantage : capital théoriquement illimité, pas de capital immobilisé
- Inconvénient : frais (~0.09% sur Solend/Kamino), transaction plus complexe, légèrement plus lente
- Recommandé quand : opportunité > capital disponible

**Option hybride (recommandée à terme) :**
Utiliser le capital propre pour les liquidations rapides et courantes, flash loan en backup pour les grosses opportunités.

---

## 4. Sources de données et APIs

### Kamino Finance

| Endpoint | Usage |
|---|---|
| `GET /v2/users/obligations` | Liste toutes les positions ouvertes |
| `GET /v2/markets` | Paramètres des marchés (thresholds, bonus) |
| SDK TypeScript officiel | Calcul on-chain du LTV, construction des instructions |

- Doc SDK : `@kamino-finance/klend-sdk` (npm)
- Les liquidation thresholds varient par token et peuvent changer via gouvernance

### Prix : Pyth Network

- Flux de prix en temps réel, directement utilisé par Kamino pour évaluer les positions
- S'abonner au même flux que Kamino garantit les mêmes prix que le protocole voit
- SDK : `@pythnetwork/client`

### Swap : Jupiter Aggregator

- Meilleur agrégateur de liquidité sur Solana
- API quote : `https://quote-api.jup.ag/v6/quote`
- Utilisé pour estimer le slippage avant d'exécuter et pour le swap post-liquidation

### Exécution prioritaire : Jito

- Jito Labs propose un système de **bundles** : plusieurs transactions atomiques avec tip aux validators
- Garantit l'inclusion dans le bloc sans concurrence (si le tip est suffisant)
- SDK : `jito-ts` (npm) ou endpoint REST `https://mainnet.block-engine.jito.wtf`

### RPC Solana

Un RPC public gratuit est trop lent. Pour un bot de liquidation, minimum :

| Fournisseur | Plan recommandé | Prix indicatif |
|---|---|---|
| Helius | Growth | ~$50–200/mois |
| Triton | Dedicated | ~$200–500/mois |
| QuickNode | Solana Add-on | ~$50–300/mois |

---

## 5. Positionnement compétitif et niches à cibler

### À éviter (trop compétitif)

- Liquidations SOL/USDC > $10,000 de profit
- Protocoles majeurs (Kamino main market) sur les paires principales
- Compétition directe avec bots MEV en Rust co-localisés

### Niches accessibles

**1. Petites liquidations ($50–$2,000 de profit)**
Les bots institutionnels les ignorent. Volume élevé, compétition faible. Rentabilité régulière.

**2. Marchés isolés / tokens secondaires**
Kamino propose des marchés isolés (ex. JitoSOL, mSOL, tokens governance). Moins surveillés, mêmes mécaniques.

**3. Fenêtres de crash**
Lors d'une chute rapide (-20% en 1h), le volume de liquidations explose. Les bots existants saturent. Des positions passent.

**4. Nouvelles intégrations Kamino**
Chaque fois qu'un nouveau token est ajouté comme collatéral, la période initiale est peu compétitive.

---

## 6. Stack technique recommandée

```
Langage       : TypeScript (Node.js)
               → Accès direct aux SDKs officiels Kamino, Jupiter, Jito
               → Suffisant pour cibler les niches petites/mid liquidations
               → Python possible mais SDK Kamino moins mature

Runtime       : Node.js 20+ avec workers pour le monitoring parallèle

Infrastructure:
  - 1 VPS Linux (DigitalOcean / Hetzner) proche des validators Solana
  - RPC dédié Helius ou Triton
  - Monitoring uptime (PagerDuty ou simple webhook Discord)

Wallet        : Keypair dédié, fonds séparés du wallet principal
               → Ne jamais mettre tout le capital sur un seul keypair

Logging       : Airtable (cohérent avec l'écosystème existant)
               ou PostgreSQL pour volumes importants

Logs de liquidation réussie :
  [UTINI !!] Liquidation exécutée
    borrower   : <wallet>
    collateral : 3.51 SOL
    debt repaid: $585 USDC
    profit     : +$46.8
    tip Jito   : 0.01 SOL
    net profit : +$44.6
```

---

## 7. Risques

| Risque | Description | Mitigation |
|---|---|---|
| Compétition MEV | Transaction rejetée car un bot plus rapide a liquidé avant | Cibler niches peu compétitives, optimiser les tips |
| Slippage swap | Le collatéral reçu vaut moins que prévu au moment du swap | Simuler le slippage avant exécution, refuser si > seuil |
| Bug smart contract | Vulnérabilité dans Kamino exploitable | Pas de risque direct pour le liquidateur — risque uniquement de perte de gas |
| Downtime du bot | Position liquidable manquée (pas grave) ou position propre non surveillée (grave) | Monitoring uptime + alertes |
| Volatilité des tips Jito | Tips trop bas = transaction ignorée, tips trop hauts = profit annulé | Algorithme dynamique basé sur l'opportunité |
| Capital bloqué | USDC immobilisé dans des swaps ratés | Timeouts stricts, gestion des erreurs robuste |

---

## 8. Étapes de développement suggérées

```
Phase 1 — Read-only (1–2 semaines)
  └─ Indexer toutes les positions Kamino
  └─ Calculer les LTV en temps réel
  └─ Logger les liquidations qui se produisent (sans les exécuter)
  └─ Objectif : comprendre la fréquence et la taille des opportunités

Phase 2 — Simulation (1–2 semaines)
  └─ Simuler les transactions de liquidation sans les envoyer
  └─ Calculer les profits théoriques
  └─ Comparer avec les liquidations réellement exécutées on-chain

Phase 3 — Exécution réelle (capital limité)
  └─ Déployer avec $1,000–$5,000 USDC max
  └─ Cibler uniquement petites liquidations ($50–$500 profit)
  └─ Valider la mécanique bout en bout

Phase 4 — Optimisation
  └─ Tuner les tips Jito
  └─ Ajouter le flash loan comme fallback
  └─ Élargir aux marchés isolés Kamino
```

---

## 9. Questions ouvertes à investiguer

- [ ] Quels sont exactement les liquidation thresholds et bonus par token sur Kamino aujourd'hui ?
- [ ] Le SDK Kamino (`klend-sdk`) permet-il de construire l'instruction de liquidation directement ?
- [ ] Quelle est la fréquence réelle de liquidations sur les petites positions (< $500 profit) sur les 6 derniers mois ?
- [ ] Est-il possible d'interroger l'historique des liquidations Kamino via leur API ou via un indexeur (Helius DAS) ?
- [ ] Jito bundles : quel est le tip minimum observé sur des liquidations mid-size récemment ?

---

*Document de travail — à enrichir au fil de la recherche*

---

> *UTINI !!*
