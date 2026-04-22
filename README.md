# Jawas — Guide de Survie (Survival Guide)

*UTINI !!* Ce guide est destiné à l'opérateur du bot Jawas.

## 🎯 Objectifs du Projet

Jawas est un bot de liquidation multi-protocole spécialisé sur **Solana**. Il surveille actuellement :
- **Kamino Finance**
- **Save Finance** (ex-Solend)

Sa stratégie repose sur deux modes distincts, activés selon la configuration :

- **Phase 1 : Spectateur (Watch/Observation)** — Mode par défaut si aucun keypair n'est fourni. Le bot observe les liquidations exécutées par d'autres acteurs sur la blockchain. Il ne dépense aucun capital et n'envoie aucune transaction.
- **Phase 2 : Chasseur de prime (Hunt/Execution)** — Activée dès que `SOLANA_KEYPAIR_PATH` est défini. Le bot exécute ses propres liquidations de manière **entièrement autonome** : chaque hunter (Kamino, Solend) possède son propre flux WebSocket QuikNode et n'est jamais bloqué par l'Observer.

---

## 📊 Glossaire Airtable (Table 'Jawas-Watch')

Chaque ligne dans Airtable représente une liquidation observée.

| Champ | Description |
| :--- | :--- |
| **Name** | Identifiant unique (WATCH + Timestamp). |
| **Tx Signature** | Signature de la transaction sur Solana (lien vers Solscan). |
| **Protocol** | Le protocole ciblé (Kamino, Solend). |
| **Market** | Le marché de prêt (Main, Jito, etc.). |
| **Liquidated User** | Adresse du portefeuille liquidé. |
| **Liquidator** | Adresse du bot ayant effectué la liquidation. |
| **Repay Mint** | Adresse de l'actif remboursé par le liquidateur. |
| **Withdraw Mint** | Adresse de l'actif récupéré (collatéral). |
| **Repay Symbol** | Symbole de l'actif remboursé (ex: USDC). |
| **Withdraw Symbol** | Symbole de l'actif récupéré (ex: SOL). |
| **Repay Amount** | Quantité d'actifs remboursés (en unités humaines). |
| **Withdraw Amount** | Quantité d'actifs récupérés (en unités humaines). |
| **Repaid USD** | Valeur estimée en USD du remboursement. |
| **Withdrawn USD** | Valeur estimée en USD du collatéral récupéré. |
| **Profit USD** | Profit brut estimé (Withdrawn - Repaid). |
| **Timestamp** | Moment de l'observation (Unix ms). |
| **Delay MS** | Temps écoulé entre la validation du bloc et la détection du bot. |
| **Competing Bots** | Nombre d'autres bots ayant tenté la même liquidation. |
| **Status** | Statut de l'observation (SUCCESS, FAILED, RPC_TIMEOUT). |

---

## 🚀 Déploiement

Le bot est conteneurisé. Chaque protocole tourne dans son propre container pour une isolation maximale.

1. **Préparation :** Assurez-vous d'avoir un fichier `.env` configuré à la racine.
2. **Lancement (Tous les protocoles) :**
   ```bash
   docker-compose up -d --build
   ```
3. **Lancement d'un protocole spécifique (ex: Kamino) :**
   ```bash
   docker-compose up -d jawas-kamino
   ```
4. **Arrêt :**
   ```bash
   docker-compose down
   ```

---

## 📜 Surveillance & Monitoring

### Lecture des Logs
Chaque container a son propre flux de logs :
```bash
docker logs -f jawas-kamino
docker logs -f jawas-solend
```

### 💓 Battement de cœur (LIFEBIT) & Architecture
Toutes les 15 minutes, le bot envoie un événement **"LIFEBIT"** dans Airtable (colonne `Tx Signature`).
- Si vous voyez un LIFEBIT récent : **Tout va bien.**
- Si le dernier LIFEBIT date de plus de 20 minutes : **Alerte.** Le bot est peut-être figé ou le RPC est déconnecté.

**Architecture des flux :**
- **Observer** : flux de logging uniquement. N'intervient jamais dans le cycle de liquidation.
- **Hunter Kamino** : pipeline multi-signal. Peut écouter en parallèle :
  - `QuickNode` via `HUNTER_RPC_URL` / `HUNTER_WS_URL`
  - `Helius` via `OBSERVER_RPC_URL` / `OBSERVER_WS_URL` comme seconde source hunter
  - `Pyth Hermes` comme source prédictive
- **Hunter Solend** : flux autonome sur le programme Solend.

Pour Kamino, la règle v1 est simple :
- première source qui gagne le lock sur une obligation -> elle seule tire
- les autres détections sont enregistrées, mais ignorées pendant la fenêtre du lock
- il n'y a **pas** de fallback pendant cette fenêtre

### Proposition d'Amélioration — Hunter Kamino
Verdict honnête :
- Le bot est **opérationnel**, mais il n'est **pas encore dominant**.
- Il peut gagner contre des opportunités peu disputées ou contre des bots faibles.
- En duel contre des bots déjà pré-armés, il part encore trop souvent **en retard**.
- Le vrai problème n'est plus "savoir envoyer une liquidation", c'est **savoir quoi tirer avant les autres** et **tirer sans relire le monde dans le hot path**.

Principe non négociable :
- Le wallet est géré manuellement. On ne chasse que les opportunités dont le `repay token` est déjà détenu.
- Le hunter Kamino ne doit jamais dépendre de l'Observer.
- Le hot path cible doit être : `QuickNode WS -> lookup mémoire -> build tx -> Jito`.

### Priorités — Impact / Difficulté
Les items sont classés du meilleur ratio impact / coût au plus lourd.

#### P0 — À faire vite
- **Supprimer tout appel RPC évitable du hot path.**
  Impact : maximal. Difficulté : moyenne.
  Tant qu'on fait encore des lectures bloquantes au moment du tir, on joue une course avec un boulet au pied.
- **Précharger les ATA du wallet, réserves, oracles et mints au démarrage.**
  Impact : très élevé. Difficulté : faible.
  C'est le gain le plus propre et le plus immédiat.
- **Ne tirer que sur les `repay mints` explicitement présents dans `wallet.toml`.**
  Impact : élevé. Difficulté : faible.
  Ça évite les branches inutiles et force une discipline de chasse.
- **Mesurer la latence réelle de bout en bout.**
  Impact : très élevé. Difficulté : faible.
  Sans timestamps sur `WS reçu -> build -> signature -> Jito`, on optimise à l'aveugle.
- **Rendre la stratégie de `priority fee` et de `Jito tip` dynamique.**
  Impact : élevé. Difficulté : moyenne.
  Un tip fixe, c'est suffisant pour perdre proprement.

État actuel :
- `getTransaction` est maintenant configurable via retries courts (`*_GET_TX_ATTEMPTS`, `*_GET_TX_RETRY_DELAY_MS`, `*_GET_TX_TIMEOUT_MS`) au lieu d'un unique tir brutal.
- Le hunter écrit désormais un log JSONL structuré (`HUNTER_LOG_FILE`) avec `ws_received`, `skip`, `error`, `firing`, `bundle_sent` et les latences associées.
- Les détails `firing`, `dry_run`, `bundle_sent` et `bundle_send_failed` incluent maintenant des timings de phase (`get_tx`, `resolve`, `prep`, `build`, `send_bundle`, `total`) pour comparer les RPC en conditions réelles.
- La sélection de l'instruction Kamino ne repose plus uniquement sur "le plus grand nombre d'accounts" : le discriminator Anchor de liquidation est validé.
- Un mode `HUNTER_DRY_RUN=true` permet de construire et signer la tx sans l'envoyer à Jito, pour tester le pipeline réel sans risque.
- L'adapter Airtable filtre maintenant les doublons par `Tx Signature` avant insertion. La dédup en mémoire de l'observer reste utile, mais l'unicité finale est garantie côté écriture.
- Le hunter pousse aussi ses propres événements dans Airtable via `Status` : `HUNTER_WS_RECEIVED`, `HUNTER_FIRING`, `HUNTER_BUNDLE_SENT`, `HUNTER_BUNDLE_FAILED`.
- Le hunter Kamino compare maintenant plusieurs sources de signal et écrit un résumé JSONL hors hot path (`HUNTER_SIGNAL_METRICS_FILE`) pour savoir quelle source a détecté quoi, et dans quel ordre.

#### P1 — Ce qui empêche de gagner souvent
- **Supprimer la dépendance au `getTransaction` du concurrent.**
  Impact : maximal. Difficulté : élevée.
  Tant qu'on reconstruit à partir de la tx d'un autre, on chasse en réaction. Les meilleurs bots sont déjà partis.
- **Maintenir un cache mémoire des cibles prioritaires.**
  Impact : maximal. Difficulté : élevée.
  Il faut connaître à l'avance : obligation, repay reserve, withdraw reserve, marché, comptes oracle.
- **Fiabiliser le scanner hebdo comme socle wallet-driven.**
  Impact : élevé. Difficulté : moyenne.
  Le wallet doit être construit sur des repay mints plausibles, pas sur du bruit RPC.
- **Durcir encore les filtres de cohérence Kamino/Solend.**
  Impact : élevé. Difficulté : moyenne.
  Si le scanner laisse passer des faux positifs, il pollue le wallet puis le hunter.

#### P2 — Ce qui fait passer de "fonctionne" à "compétitif"
- **Construire une vraie watchlist propriétaire des obligations proches du seuil.**
  Impact : maximal. Difficulté : élevée.
  C'est là que se crée l'avantage informationnel.
- **Faire évoluer le hunter Kamino vers un mode réellement pré-armé.**
  Impact : maximal. Difficulté : élevée.
  Le but est de ne plus découvrir la cible pendant le tir.
- **Ajuster l'infra pour minimiser la distance au leader et la variance réseau.**
  Impact : élevé. Difficulté : élevée.
  Si l'infra est lente, le code ne sauvera pas la course.

#### P3 — Important mais secondaire
- **Rendre le scanner hebdo plus lisible pour la préparation du wallet.**
  Impact : moyen. Difficulté : faible.
  Le rapport doit rester orienté `repay mint`, pas curiosité analytique.
- **Élargir le catalogue token local.**
  Impact : moyen. Difficulté : faible.
  C'est du confort opérationnel, pas un edge.
- **Nettoyer les outils d'inspection et les rapprocher du comportement on-chain réel.**
  Impact : moyen. Difficulté : faible.
  Utile pour diagnostiquer, pas pour gagner une course.
- **AutoSwap après liquidation.**
  Impact : moyen. Difficulté : moyenne.
  Si un tir gagne, il faut pouvoir swapper automatiquement le collatéral reçu vers un actif cible défini par `.env` et autorisé dans `wallet.toml` (exemple : `USDC`). Ce n'est pas ce qui fait gagner le duel, mais c'est ce qui évite de finir avec un wallet poubelle.

### Ce qu'il faut accepter
- Un bot "réactif rapide" peut liquider.
- Un bot "pré-armé" gagne les duels.
- Aujourd'hui, le projet est entre les deux.
- Si l'objectif est de gagner souvent, la cible n'est pas "faire plus de features". La cible est : **moins de lectures, moins de branches, moins de dépendances, plus de préparation mémoire**.

### Outils d'Inspection
Commandes utiles pour diagnostiquer une obligation à la main :

```bash
cargo run --bin inspect_kamino_obligation <OBLIGATION_PUBKEY>
cargo run --bin inspect_solend_obligation <OBLIGATION_PUBKEY>
cargo run --bin generate_weekly_token_report
```

Usage :
- `inspect_kamino_obligation` : affiche les agrégats Kamino on-chain et tente un refresh simulé.
- `inspect_solend_obligation` : affiche les agrégats Solend décodés depuis le compte obligation.
- `generate_weekly_token_report` : scanne on-chain les obligations Kamino et Solend proches du seuil de liquidation, agrège les `repay tokens` les plus probables à stocker en wallet, les paires associées, puis insère une ligne dans `jawas-weekly-token`.
  Le scanner exclut les comptes non plausibles et les positions dont les réserves ne se résolvent pas proprement vers un mint de token.

Variables utiles :
- `AIRTABLE_TABLE_WEEKLY_TOKEN` : table cible, par défaut `jawas-weekly-token`.
- `WEEKLY_REPORT_MIN_COLLATERAL_USD` : seuil minimum en USD pour conserver une obligation dans le rapport hebdo. Par défaut `1`. Le filtre est appliqué sur la valeur collatérale on-chain de l'obligation pour éliminer les positions trop petites.
- `WEEKLY_REPORT_MIN_BORROW` : ancien nom encore accepté comme fallback, mais à remplacer.
- `WEEKLY_REPORT_TOP_N` : nombre de positions proches retenues pour l'agrégation. Par défaut `50`.
- `WEEKLY_REPORT_SHORTLIST_SIZE` : nombre de `repay tokens` à afficher dans `Shortlist`. Par défaut `10`. Le champ est formaté avec une pondération relative et le mint complet, par exemple `USDC [EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v] 42.0% (21)`.
- `HUNTER_LOG_FILE` : chemin du log JSONL du hunter. Par défaut `hunter_trace.jsonl`. Mettre `off` pour désactiver.
- `HUNTER_SIGNAL_METRICS_FILE` : résumé JSONL des locks Kamino multi-signal. Par défaut `hunter_signal_metrics.jsonl`. Mettre `off` pour désactiver.
- `HUNTER_SIGNAL_LOCK_MS` : durée du lock "first wins" sur une obligation Kamino. Par défaut `1500`.
- `HUNTER_DRY_RUN` : si `true`, le hunter va jusqu'au build/sign de la tx puis s'arrête avant `sendBundle`. Le log contient alors un événement `dry_run`.
- `HUNTER_REPLAY` : si `true`, Jawas exécute un replay one-shot du hunter au boot puis s'arrête.
- `HUNTER_REPLAY_SIGNATURE` : signature à rejouer. Si absente en mode Kamino, Jawas utilise un fallback codé en dur (`3V11m9fyEiUqbrihZPF1QJdXW9g6tr4mHS9VtCS2BNSunUeQWvRTgXf48uoC7gXgij8bKp7hSERZ1CZvNhSYgCLA`).
- `ENABLE_HUNTER_SIGNAL_QUICKNODE` : active la source Kamino QuickNode. Par défaut `true`.
- `ENABLE_HUNTER_SIGNAL_HELIUS` : active la source Kamino Helius en réutilisant `OBSERVER_RPC_URL` / `OBSERVER_WS_URL`. Par défaut `true`.
- `ENABLE_HUNTER_SIGNAL_HERMES` : active la source Kamino Pyth Hermes. Par défaut `false`.
- `HERMES_WS_URL` : endpoint Hermes. Par défaut `https://hermes.pyth.network`.
- `HERMES_SHORTLIST_SIZE` : taille maximale de la shortlist Kamino surveillée par Hermes. Par défaut `200`.
- `HERMES_REFRESH_SECS` : fréquence de refresh de l'état Kamino utilisé par Hermes. Par défaut `10`.
- `HERMES_TRIGGER_BUFFER_BPS` : buffer prédictif Hermes avant émission du signal. Par défaut `25`.
- `HUNTER_GET_TX_ATTEMPTS`, `HUNTER_GET_TX_RETRY_DELAY_MS`, `HUNTER_GET_TX_TIMEOUT_MS` : réglages globaux du fetch `getTransaction`.
- `KAMINO_GET_TX_ATTEMPTS`, `KAMINO_GET_TX_RETRY_DELAY_MS`, `KAMINO_GET_TX_TIMEOUT_MS` : overrides spécifiques Kamino.
- `SOLEND_GET_TX_ATTEMPTS`, `SOLEND_GET_TX_RETRY_DELAY_MS`, `SOLEND_GET_TX_TIMEOUT_MS` : overrides spécifiques Solend.
- `BLOCKHASH_REFRESH_SECS` : rester pragmatique. Un refresh trop agressif peut exploser le quota QuickNode. Une valeur autour de `12` secondes est acceptable si le budget API est serré.

Tests utiles :
```bash
cargo check
cargo test hunter::tests -- --nocapture
```

Exemple de replay local Kamino :
```bash
HUNTER_REPLAY=true HUNTER_DRY_RUN=true SOLANA_KEYPAIR_PATH=secrets/keypair.json cargo run --bin jawas
```

---

## 🧠 Guide d'Analyse Stratégique (Phase 1)

L'objectif de la Phase 1 est de répondre à une question : **"Pouvons-nous gagner de l'argent sans nous faire écraser par les institutions ?"**

### L'analogie du Photographe
En Phase 1, Jawas est un **photographe sur le bord d'une piste de course**. Il ne court pas, il observe les autres courir :
- Une transaction **SUCCESS** sur Solscan = Le coureur qui a franchi la ligne en premier et empoché la prime.
- Une transaction **FAILED** sur Solscan = Un coureur qui a essayé, mais qui est arrivé quelques millisecondes trop tard.

### Comment analyser vos données Airtable ?

Après 1 ou 2 semaines, filtrez vos données pour identifier les opportunités :

#### 1. Chercher le "Gisement" (Niches)
Filtrez le champ `Profit USD` entre **$50 et $500**. 
- **Beaucoup de SUCCESS dans cette zone ?** C'est bon signe. Cela veut dire que des liquidations de petite taille se produisent régulièrement.
- **Peu de FAILED pour ces transactions ?** C'est encore mieux ! Cela signifie que la compétition est faible sur ces montants.

#### 2. Mesurer la "Férocité" (Compétition)
Regardez une signature `SUCCESS` et cherchez si d'autres lignes Airtable ont le même `Liquidated User` au même moment mais sont `FAILED`.
- **10 FAILED pour 1 SUCCESS** : La zone est ultra-compétitive (bots institutionnels). Danger.
- **0 ou 1 FAILED pour 1 SUCCESS** : La zone est calme. C'est notre cible prioritaire pour la Phase 2.

#### 3. Valider notre "Vitesse" (Delay MS)
Regardez le champ `Delay MS`. Il indique combien de temps après l'arrivée du signal WebSocket notre bot a fini de traiter l'info.
- **Moins de 50ms** : Notre code est rapide. Nous sommes prêts techniquement.
- **Plus de 200ms** : Nous devrons optimiser le code ou changer de serveur avant de passer en Phase 2.

### Le signal du "Go / No-Go"
Vous ne devriez passer en **Phase 2 (Hunt)** que si vous trouvez au moins **3 niches par jour** où :
1. Le profit est > $50.
2. Il y a moins de 2 bots concurrents (FAILED) sur la même opportunité.
3. Votre `Delay MS` moyen est stable.

---

## 🔍 Outils de Diagnostic (Cross-Check)

Si vous ne voyez aucune liquidation dans Airtable, vous pouvez vérifier manuellement si le marché est calme ou si le bot a un problème avec l'outil historique :

### Script de Cross-Check Historique
Ce script Python scanne les dernières transactions réelles de Kamino et génère un rapport Markdown.

1. **Installation :**
   ```bash
   pip install requests python-dotenv
   ```
2. **Lancement :**
   ```bash
   python tools/kamino_history.py
   ```
3. **Analyse :**
   - Si le script trouve des liquidations que le bot n'a pas vues : **Le bot a un bug de parsing.**
   - Si le script ne trouve rien non plus : **Le marché est simplement calme.**

---

## 🛠 Dépannage (Troubleshooting)

| Problème | Solution |
| :--- | :--- |
| **Pas de logs ?** | Vérifiez le statut du conteneur : `docker ps`. Relancez avec `docker-compose restart`. |
| **Airtable vide ?** | Vérifiez vos tokens dans `.env` et assurez-vous que `AIRTABLE_TABLE_WATCH` correspond au nom de votre table. |
| **Erreur RPC (429/Too Many Requests) ?** | Votre endpoint QuickNode est saturé. Changez-le dans `.env`. |
| **Le bot s'arrête tout seul ?** | Consultez les logs (`docker logs jawas`) pour identifier l'erreur fatale (souvent un problème de configuration). |

---

## ⚙️ Configuration (.env)

Jawas utilise une stratégie de double-flux RPC pour optimiser les performances et les coûts.

### 1. Variables d'environnement
Créer un fichier `.env` à la racine :

```bash
# Airtable Config
AIRTABLE_TOKEN=votre_token
AIRTABLE_BASE_ID=appmvsotfJe4SO1Ll
AIRTABLE_TABLE_WATCH=Jawas-Watch

# Flux OBSERVATION + signal Helius secondaire pour Kamino
OBSERVER_RPC_URL=https://mainnet.helius-rpc.com/?api-key=votre_cle
OBSERVER_WS_URL=wss://mainnet.helius-rpc.com/?api-key=votre_cle

# Flux CHASSE / HUNTER principal (QuickNode recommandé)
HUNTER_RPC_URL=https://votre-endpoint-quicknode.com/
HUNTER_WS_URL=wss://votre-endpoint-quicknode.com/
JITO_URL=https://mainnet.block-engine.jito.wtf/api/v1/bundles

# Optionnel: override manuel de la liste des 8 tip accounts Jito
# Sinon Jawas utilise la liste officielle intégrée et en choisit un pseudo-aléatoirement.
JITO_TIP_ACCOUNTS=3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT,HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe

# Keypair (Phase 2)
SOLANA_KEYPAIR_PATH=secrets/keypair.json

# Activation fine des services
ENABLE_HUNTER=true
ENABLE_OBSERVER=true

# Activation fine des sources Kamino
ENABLE_HUNTER_SIGNAL_QUICKNODE=true
ENABLE_HUNTER_SIGNAL_HELIUS=true
ENABLE_HUNTER_SIGNAL_HERMES=false

# Lock "première source gagnante"
HUNTER_SIGNAL_LOCK_MS=1500

# Logs et métriques JSONL
HUNTER_LOG_FILE=hunter_trace.jsonl
HUNTER_SIGNAL_METRICS_FILE=hunter_signal_metrics.jsonl

# Source Hermes (prédictive)
HERMES_WS_URL=https://hermes.pyth.network
HERMES_SHORTLIST_SIZE=200
HERMES_REFRESH_SECS=10
HERMES_TRIGGER_BUFFER_BPS=25
```

### 2. Choix des fournisseurs RPC
*   **Helius** : utile comme flux de watch et comme seconde source hunter Kamino indépendante.
*   **QuickNode** : reste la source hunter principale et le RPC de tir par défaut.
*   **Pyth Hermes** : sert à construire un signal prédictif Kamino basé sur une shortlist, et reste désactivé par défaut tant que sa validation n'est pas jugée suffisante.

### Configuration Avancée (Multi-Tables)
Par défaut, tous les containers envoient leurs données dans la même table Airtable (identifiée par le champ `Protocol`). Si vous souhaitez utiliser des tables séparées :
1. Modifiez le fichier `docker-compose.yml`.
2. Ajoutez la variable `AIRTABLE_TABLE_WATCH` sous la section `environment` du service concerné.

Exemple pour Solend :
```yaml
  jawas-solend:
    environment:
      - TARGET_PROTOCOL=SOLEND
      - ENABLE_HUNTER=false
      - ENABLE_OBSERVER=true
      - AIRTABLE_TABLE_WATCH=Jawas-Watch-Solend
```

---

> *"Ces droïdes... ils ramassent tout ce qui traîne."*
