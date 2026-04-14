# Jawas — Guide de Survie (Survival Guide)

*UTINI !!* Ce guide est destiné à l'opérateur du bot Jawas.

## 🎯 Objectifs du Projet

Jawas est un bot de liquidation multi-protocole spécialisé sur **Solana**. Il surveille actuellement :
- **Kamino Finance**
- **Save Finance** (ex-Solend)

Sa stratégie repose sur deux phases distinctes :

- **Phase 1 : Spectateur (Watch/Observation)** — Actuellement active. Le bot observe les liquidations exécutées par d'autres acteurs sur la blockchain. Il ne dépense aucun capital et n'envoie aucune transaction. L'objectif est de collecter des données réelles pour valider la rentabilité d'une future Phase 2.
- **Phase 2 : Chasseur de prime (Hunt/Execution)** — Non active. Le bot exécutera ses propres liquidations de manière autonome une fois la Phase 1 jugée concluante.

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
   docker-compose up -d jawas-watch-kamino
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
docker logs -f jawas-watch-kamino
docker logs -f jawas-watch-solend
```

### 💓 Battement de cœur (LIFEBIT) & Watchdog
Toutes les 15 minutes, le bot envoie un événement **"LIFEBIT"** dans Airtable (colonne `Tx Signature`).
- Si vous voyez un LIFEBIT récent : **Tout va bien.**
- Si le dernier LIFEBIT date de plus de 20 minutes : **Alerte.** Le bot est peut-être figé ou le RPC est déconnecté.

**Nouveauté (Fiabilité) :** Le bot inclut désormais un **Watchdog RPC**. Si aucune donnée n'est reçue pendant 2 minutes, le bot considère la connexion comme "zombie" et force automatiquement une reconnexion pour ne rater aucune liquidation.

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

# Flux OBSERVATION (Recommandé : Helius)
# Utilisé pour surveiller les transactions des concurrents (WebSocket intensif)
OBSERVER_RPC_URL=https://mainnet.helius-rpc.com/?api-key=votre_cle
OBSERVER_WS_URL=wss://mainnet.helius-rpc.com/?api-key=votre_cle

# Flux CHASSE / HUNTER (Recommandé : QuickNode)
# Utilisé pour simuler et envoyer nos propres liquidations (Vitesse critique)
HUNTER_RPC_URL=https://votre-endpoint-quicknode.com/
HUNTER_WS_URL=wss://votre-endpoint-quicknode.com/

# Keypair (Phase 2)
SOLANA_KEYPAIR_PATH=/app/secrets/keypair.json
```

### 2. Choix des fournisseurs RPC
*   **Helius (Observer)** : Idéal pour le flux de "Watch" grâce à sa gestion très robuste des WebSockets et son plan gratuit généreux.
*   **QuickNode (Hunter)** : Recommandé pour la Phase 2 (Hunt) pour sa faible latence lors de l'envoi des transactions et ses options de Priority Fees.

### Configuration Avancée (Multi-Tables)
Par défaut, tous les containers envoient leurs données dans la même table Airtable (identifiée par le champ `Protocol`). Si vous souhaitez utiliser des tables séparées :
1. Modifiez le fichier `docker-compose.yml`.
2. Ajoutez la variable `AIRTABLE_TABLE_WATCH` sous la section `environment` du service concerné.

Exemple pour Solend :
```yaml
  jawas-watch-solend:
    environment:
      - TARGET_PROTOCOL=SOLEND
      - AIRTABLE_TABLE_WATCH=Jawas-Watch-Solend
```

---

> *"Ces droïdes... ils ramassent tout ce qui traîne."*
