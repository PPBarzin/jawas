# Comprendre RPC, WSS, Helius, QuickNode et le reste

> Statut : note pédagogique
>  
> Date : 2026-05-05
>  
> Public visé : débutant qui veut comprendre le vocabulaire et les concepts avant de raisonner sur Jawas

---

## 1. L'idée simple avant tout

Quand Jawas "parle à Solana", il ne parle pas directement à toute la blockchain comme dans un film.

En pratique :

1. Jawas envoie des requêtes à un **nœud Solana**
2. ce nœud expose une **API**
3. cette API est souvent une **JSON-RPC API**
4. l'accès se fait soit en **HTTP/HTTPS**, soit en **WebSocket/WSS**

La phrase la plus utile à retenir est donc :

> **RPC = la manière de demander quelque chose à un nœud**
>
> **WSS = la manière de garder une ligne ouverte pour recevoir des événements en temps réel**

---

## 2. C'est quoi un nœud ?

Un **nœud** est une machine qui participe au réseau Solana ou qui en suit l'état.

Ce nœud sait :

- lire l'état des comptes
- lire les transactions
- suivre les slots et les blocs
- simuler ou relayer des transactions

Ton bot ne veut pas reconstruire tout Solana lui-même. Il délègue donc ce travail à un nœud accessible à distance.

---

## 3. C'est quoi RPC ?

**RPC** veut dire **Remote Procedure Call**.

En français simple :

> ton programme appelle à distance une fonction exposée par un serveur

Exemple mental :

- tu veux le solde d'un compte
- tu appelles une méthode RPC comme `getBalance`
- le nœud répond avec la donnée

Le mot important ici est **méthode**.

Une méthode RPC, c'est juste une commande standardisée du style :

- `getBalance`
- `getAccountInfo`
- `getTransaction`
- `sendTransaction`
- `simulateTransaction`

Sur Solana, ces méthodes sont généralement exposées en **JSON-RPC**.

---

## 4. C'est quoi JSON-RPC ?

**JSON-RPC** est un protocole très simple :

- tu envoies un objet JSON
- tu précises la méthode
- tu ajoutes les paramètres
- le serveur renvoie un résultat ou une erreur

Exemple conceptuel :

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "getBalance",
  "params": ["adresse_du_compte"]
}
```

Le serveur répond avec quelque chose du même style :

```json
{
  "jsonrpc": "2.0",
  "result": {
    "value": 123456789
  },
  "id": 1
}
```

Ce n'est pas magique. C'est juste une convention de dialogue.

---

## 5. HTTP RPC vs WebSocket RPC

Il faut bien séparer ces deux usages.

### HTTP RPC

C'est le mode :

- j'envoie une requête
- j'attends une réponse
- la connexion se termine

C'est très bien pour :

- lire un compte maintenant
- demander une transaction précise
- envoyer une transaction
- simuler une transaction

Exemples de méthodes souvent utilisées en HTTP :

- `getAccountInfo`
- `getMultipleAccounts`
- `getProgramAccounts`
- `getLatestBlockhash`
- `getSignatureStatuses`
- `sendTransaction`
- `simulateTransaction`

### WSS / WebSocket RPC

C'est le mode :

- j'ouvre une connexion durable
- je m'abonne à un flux
- le serveur me pousse des notifications quand quelque chose change

C'est très bien pour :

- surveiller un compte
- écouter les logs d'un programme
- suivre des transactions en temps réel
- réagir vite à un événement

Exemples de méthodes d'abonnement :

- `accountSubscribe`
- `programSubscribe`
- `logsSubscribe`
- `signatureSubscribe`
- `slotSubscribe`
- `transactionSubscribe`

La logique à retenir :

> **HTTP sert surtout à demander**
>
> **WSS sert surtout à écouter**

---

## 6. Pourquoi on dit parfois RPC et parfois WSS ?

Parce qu'en pratique, beaucoup de gens utilisent "RPC" pour parler de tout l'accès à un provider.

Exemple courant :

- "on a changé de RPC"

Souvent, cela veut dire :

- nouvel endpoint HTTP
- et parfois aussi nouvel endpoint WSS

Techniquement, c'est un raccourci.

Plus précisément :

- **RPC** = le mécanisme d'appel de méthodes
- **HTTP RPC** = appels ponctuels
- **WSS RPC** = abonnements temps réel

---

## 7. Le rôle d'un provider comme Helius ou QuickNode

Helius et QuickNode ne sont pas "la blockchain".

Ce sont des **providers d'infrastructure**.

Ils te vendent ou te fournissent :

- des nœuds Solana accessibles
- des endpoints HTTPS
- des endpoints WSS
- du monitoring
- parfois des APIs enrichies
- parfois des produits de streaming plus spécialisés

En gros, ils te disent :

> "Ne maintiens pas toi-même ton infra Solana, utilise la nôtre."

Donc :

- **Solana** définit les méthodes standards
- **Helius/QuickNode** exposent ces méthodes
- et ajoutent parfois des produits maison autour

---

## 8. Ce qui est standard Solana vs ce qui est spécifique au provider

C'est une distinction essentielle.

### Standard Solana

Ce sont les méthodes que beaucoup de nœuds Solana exposent de manière comparable :

- `getAccountInfo`
- `getProgramAccounts`
- `sendTransaction`
- `logsSubscribe`
- `programSubscribe`
- `accountSubscribe`

Si ton code n'utilise que cela, il est en général plus portable.

### Spécifique au provider

Ce sont les produits ou méthodes ajoutés par un provider :

- APIs enrichies
- webhooks
- streams managés
- gRPC spécialisé
- enrichissement de transactions
- filtres avancés

Si ton code dépend de cela, il devient plus efficace ou plus pratique, mais aussi moins portable.

---

## 9. Helius, en termes simples

Helius est très utilisé dans l'écosystème Solana pour :

- RPC standard
- WSS standard
- APIs enrichies
- webhooks
- produits orientés streaming faible latence

À la date du 2026-05-05, la documentation officielle Helius met notamment en avant :

- les méthodes WSS standard Solana sur leur infra
- des WebSockets "enhanced" autour de `transactionSubscribe` et `accountSubscribe`
- `LaserStream`, présenté comme un produit de streaming faible latence avec replay historique court

L'idée pédagogique :

> Helius ne fait pas seulement "un nœud RPC".
>
> Helius essaie aussi de vendre une couche "données temps réel pour bots et analytics".

---

## 10. QuickNode, en termes simples

QuickNode est aussi un provider d'infrastructure multi-chaînes, très présent sur Solana.

Ils proposent en général :

- RPC standard
- WSS standard
- add-ons ou produits complémentaires
- outils de streaming managés
- options de type Yellowstone gRPC

À la date du 2026-05-05, leur documentation officielle expose notamment :

- les méthodes WSS Solana standard comme `logsSubscribe`, `programSubscribe`, `transactionSubscribe`
- un produit `Streams`
- un produit `Yellowstone gRPC` pour le streaming haute performance

L'idée pédagogique :

> QuickNode vend une plateforme d'accès à la chaîne, avec des produits additionnels selon les besoins de débit, de filtrage et d'exploitation des données.

---

## 11. WSS n'est pas "plus fort" que RPC

Erreur classique du débutant :

> "WSS remplace RPC"

Non.

En pratique, un bot sérieux utilise souvent les deux.

### Exemple typique

1. via **WSS**, le bot reçoit un signal : "quelque chose a changé"
2. via **HTTP RPC**, il récupère ensuite des données détaillées
3. via **HTTP RPC**, il simule ou envoie une transaction

Donc :

- **WSS détecte**
- **HTTP confirme et agit**

---

## 12. Les grandes familles de méthodes à connaître

Voici les familles les plus utiles pour raisonner correctement.

### Lire l'état

Exemples :

- `getAccountInfo`
- `getMultipleAccounts`
- `getProgramAccounts`

Question à laquelle elles répondent :

> "À quoi ressemble l'état on-chain maintenant ?"

### Lire l'historique ou une transaction

Exemples :

- `getTransaction`
- `getSignatureStatuses`
- `getSignaturesForAddress`

Question :

> "Qu'est-ce qui s'est passé ?"

### Préparer l'envoi

Exemples :

- `getLatestBlockhash`
- `simulateTransaction`

Question :

> "Si j'envoie ça, que va-t-il probablement se passer ?"

### Envoyer

Exemple :

- `sendTransaction`

Question :

> "Peux-tu relayer ma transaction au réseau ?"

### Écouter en temps réel

Exemples :

- `logsSubscribe`
- `programSubscribe`
- `accountSubscribe`
- `transactionSubscribe`
- `slotSubscribe`

Question :

> "Préviens-moi quand quelque chose d'intéressant se produit."

---

## 13. `logsSubscribe`, `programSubscribe`, `accountSubscribe` : la différence

Ces trois méthodes sont souvent confondues.

### `accountSubscribe`

Tu surveilles **un compte précis**.

Exemple :

- un token account
- un wallet
- un compte de position

Tu reçois une notification quand les données de ce compte changent.

Question mentale :

> "Dis-moi si CET objet change."

### `programSubscribe`

Tu surveilles **tous les comptes appartenant à un programme**.

Exemple :

- tous les comptes gérés par Kamino
- tous les marchés d'un protocole

Question mentale :

> "Dis-moi si un objet géré par CE programme change."

Très puissant, mais peut être très bruyant.

### `logsSubscribe`

Tu surveilles les **logs produits par les transactions**.

Exemple :

- voir si une instruction de liquidation a été exécutée
- détecter qu'un programme a été invoqué

Question mentale :

> "Dis-moi quand une transaction laisse cette trace dans ses logs."

Pour Jawas, cette méthode est souvent utile pour détecter rapidement une activité autour d'un programme cible.

---

## 14. `transactionSubscribe` : pourquoi tout le monde en parle

`transactionSubscribe` est utile quand tu veux recevoir des mises à jour transactionnelles plus structurées.

C'est souvent plus proche de :

> "Montre-moi les transactions qui matchent tel filtre"

que de :

> "Montre-moi juste les logs textuels"

Cela peut être très pratique pour :

- surveiller des comptes mentionnés dans les transactions
- récupérer plus directement le contexte transactionnel
- éviter une partie du travail de reconstruction

Mais attention :

- plus de confort ne veut pas dire gratuit
- plus de détails veut souvent dire plus de volume de données
- plus de volume veut souvent dire plus de coût et plus de charge côté bot

---

## 15. Commitment : `processed`, `confirmed`, `finalized`

C'est une notion centrale.

### `processed`

Le plus rapide.

Mais aussi le moins sûr.

Idée :

> "Un validateur l'a vu et traité, mais ce n'est pas encore solidement ancré."

Usage :

- détection très rapide
- bots réactifs
- systèmes qui acceptent un peu de bruit

### `confirmed`

Compromis classique.

Idée :

> "Le réseau a déjà sérieusement reconnu cet état."

Usage :

- beaucoup d'applications normales
- surveillance raisonnablement fiable

### `finalized`

Le plus sûr, mais le plus tardif.

Idée :

> "On considère cela comme solidement finalisé."

Usage :

- reporting
- comptabilité
- vérification finale

Règle simple :

> plus tu veux aller vite, plus tu acceptes de l'incertitude

---

## 16. Latence, bruit, fiabilité : le vrai triangle

Quand tu choisis une méthode ou un provider, tu arbitres souvent entre :

- **latence** : recevoir l'info très vite
- **bruit** : recevoir beaucoup d'événements pas si utiles
- **fiabilité** : recevoir quelque chose de stable et complet

Exemples :

- `processed` donne vite l'info, mais peut être plus bruité
- `programSubscribe` peut être très riche, mais très bavard
- `logsSubscribe` peut être rapide, mais parfois moins structuré

Un bon design de bot ne cherche pas "la meilleure méthode absolue".

Il cherche :

> **la bonne combinaison pour le bon usage**

---

## 17. Où se place gRPC dans cette histoire ?

Tu verras souvent passer un troisième mot : **gRPC**.

L'idée simple :

- JSON-RPC via HTTP/WSS est le standard courant pour parler à un nœud
- gRPC est un autre style d'interface, souvent utilisé pour du streaming plus performant et plus structuré

Dans l'univers Solana, `Yellowstone gRPC` est souvent cité pour :

- streaming faible latence
- gros volumes
- filtrage plus fin
- cas orientés bots ou indexeurs

En simplifiant beaucoup :

- **HTTP JSON-RPC** : requêtes ponctuelles
- **WSS JSON-RPC** : abonnements standard
- **gRPC / Yellowstone** : streaming plus spécialisé et souvent plus "industriel"

---

## 18. Webhooks, Streams, Enhanced APIs : attention au vocabulaire marketing

Les providers ajoutent souvent leurs propres couches.

### Webhook

Au lieu de garder ton socket ouvert, le provider t'envoie un appel HTTP quand un événement arrive.

Bien pour :

- intégrations simples
- pipelines back-end

Moins naturel pour :

- ultra faible latence
- bots de compétition

### Streams

Nom souvent utilisé pour des produits managés de streaming de données.

Cela veut souvent dire :

- filtrage
- livraison temps réel
- éventuellement replay ou backfill
- intégration vers d'autres systèmes

### Enhanced API

Cela veut souvent dire :

- données enrichies
- format plus pratique
- moins de décodage à faire côté client

Mais attention :

> plus c'est "enhanced", plus tu dépends du provider

---

## 19. Ce que Jawas a besoin de comprendre concrètement

Pour Jawas, les questions utiles ne sont pas seulement "quelle méthode existe ?".

Les vraies questions sont :

### 1. Comment détecter un signal utile le plus tôt possible ?

Exemples :

- `logsSubscribe`
- `transactionSubscribe`
- `programSubscribe`

### 2. Comment confirmer rapidement l'état réel ?

Exemples :

- `getAccountInfo`
- `getMultipleAccounts`
- `getProgramAccounts`
- parfois simulation

### 3. Comment envoyer ensuite une transaction avec le moins de retard possible ?

Exemples :

- `sendTransaction`
- provider rapide
- chemin d'envoi correct
- gestion des priorités et du blockhash

### 4. Quel est le coût caché ?

Exemples :

- bruit WSS
- limites de débit
- volume de données
- coût provider
- complexité logicielle

---

## 20. Exemple de chaîne mentale correcte pour un bot

Voici une séquence simple et saine :

1. ouvrir un flux WSS
2. détecter un événement pertinent
3. recharger les comptes utiles en HTTP RPC
4. recalculer localement la situation
5. simuler si nécessaire
6. envoyer la transaction
7. suivre le statut jusqu'au résultat

Ce modèle évite une erreur fréquente :

> croire qu'un seul abonnement WSS suffit à tout faire proprement

En général, non.

---

## 21. Les erreurs classiques d'un débutant

### Erreur 1

Confondre **provider** et **protocole**.

Helius n'est pas Solana. QuickNode n'est pas Solana.

### Erreur 2

Confondre **RPC** et **HTTP uniquement**.

Le RPC peut être exposé aussi via WSS.

### Erreur 3

Croire que **WSS = plus rapide donc toujours meilleur**.

Non. Il faut encore relire, recalculer, filtrer et agir correctement.

### Erreur 4

Croire que plus de données = meilleure stratégie.

Souvent, plus de données = plus de bruit et plus de temps de traitement.

### Erreur 5

Ignorer le niveau de **commitment**.

Or c'est lui qui change radicalement le compromis vitesse / fiabilité.

---

## 22. Petit glossaire propre

### RPC

Mécanisme d'appel de méthodes à distance.

### JSON-RPC

Format standard des requêtes/réponses RPC en JSON.

### HTTP RPC

Appels ponctuels requête/réponse.

### WebSocket / WSS

Connexion persistante pour abonnements temps réel.

### Endpoint

URL du service, par exemple un point d'entrée HTTPS ou WSS.

### Subscription

Abonnement à un flux d'événements.

### Node

Machine qui expose l'état ou la connectivité au réseau.

### Provider

Entreprise qui héberge et vend cet accès.

### Commitment

Niveau de confiance/finalité demandé pour les données.

### Latency

Temps entre l'événement réseau et sa réception/traitement.

### Replay / Backfill

Capacité à rejouer une partie des données passées après une coupure.

### Enhanced

Fonctionnalité non strictement standard, ajoutée par un provider.

---

## 23. Résumé ultra simple

Si tu ne devais retenir que cela :

1. **Solana** définit des méthodes standard pour lire, écouter et envoyer.
2. **Helius** et **QuickNode** sont des fournisseurs d'accès à ces méthodes.
3. **HTTP RPC** sert surtout à demander une information ou envoyer une transaction.
4. **WSS** sert surtout à rester branché sur les événements en temps réel.
5. Un bot sérieux utilise souvent **WSS pour détecter** et **HTTP RPC pour confirmer et agir**.
6. Les produits "enhanced", "streams" ou "gRPC" sont des couches supplémentaires, utiles, mais plus spécifiques au provider.

---

## 24. Sources officielles consultées

Sources vérifiées le 2026-05-05 pour éviter d'écrire quelque chose d'obsolète sur les offres provider :

- Helius WebSocket docs : https://www.helius.dev/docs/rpc/websocket
- Helius WebSocket methods : https://www.helius.dev/docs/api-reference/rpc/websocket-methods
- Helius LaserStream : https://www.helius.dev/docs/laserstream
- Helius Enhanced `transactionSubscribe` : https://www.helius.dev/docs/enhanced-websockets/transaction-subscribe
- QuickNode `logsSubscribe` : https://www.quicknode.com/docs/solana/logsSubscribe
- QuickNode `programSubscribe` : https://www.quicknode.com/docs/solana/programSubscribe
- QuickNode `transactionSubscribe` : https://www.quicknode.com/docs/solana/transactionSubscribe
- QuickNode Yellowstone gRPC overview : https://www.quicknode.com/docs/solana/yellowstone-grpc/overview/
- QuickNode Streams : https://www.quicknode.com/docs/streams

---

## 25. Suite logique pour Jawas

La prochaine bonne étape serait une seconde note, beaucoup plus appliquée, intitulée par exemple :

`comment Jawas doit utiliser RPC/WSS selon ses besoins réels`

Elle pourrait répondre à des questions concrètes :

- quelle méthode utiliser pour détecter une liquidation concurrente
- quand préférer `logsSubscribe` à `transactionSubscribe`
- quand un provider standard suffit
- à partir de quand un flux type Yellowstone ou LaserStream devient pertinent
- où se trouvent les vrais goulets d'étranglement : réseau, décodage, simulation, envoi, ou concurrence
