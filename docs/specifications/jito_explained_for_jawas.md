# Comprendre Jito dans Jawas

> Statut : note pédagogique
>
> Date : 2026-05-14
>
> Public visé : mainteneur de Jawas qui veut comprendre ce qu'est Jito, ce que Jawas lui envoie, et ce que cela change dans un bot de liquidation Solana

---

## 1. L'idée simple avant tout

Quand Jawas veut tirer une liquidation, il doit faire plus que "construire une transaction".

Il doit aussi répondre à une question pratique :

> **comment faire parvenir cette transaction au bon endroit, au bon moment, avec assez de priorité pour qu'elle ait une chance d'être incluse avant les autres ?**

Dans Jawas, Jito est une partie de cette réponse.

La phrase la plus utile à retenir est donc :

> **Jito est une infrastructure de soumission prioritaire de transactions sur Solana**

Et dans Jawas :

> **Jito est le chemin principal utilisé pour envoyer un tir de liquidation**

---

## 2. Le problème que Jito essaie de résoudre

Sur Solana, construire une bonne transaction ne suffit pas.

Même si ton bot :

- détecte la bonne opportunité
- calcule le bon repay
- construit la bonne instruction

il reste encore un problème :

> d'autres bots essaient d'envoyer eux aussi presque en même temps

Donc il faut penser non seulement en termes de **justesse**, mais aussi en termes de :

- vitesse
- ordre d'arrivée
- priorité économique

Jito existe dans cet espace-là.

---

## 3. Vue très simple du rôle de Jito

En lecture pédagogique, tu peux voir Jito comme ceci :

1. ton bot prépare une ou plusieurs transactions
2. il les envoie au **Jito Block Engine**
3. ces transactions sont soumises avec un **tip**
4. ce tip sert à rendre l'exécution plus attractive dans le circuit de block building

L'idée générale est :

> "je ne veux pas seulement envoyer ma transaction à un nœud RPC standard"
>
> "je veux un chemin plus explicitement orienté compétition / priorité"

---

## 4. C'est quoi un bundle ?

Le mot important côté Jito est **bundle**.

Un bundle est, en première approximation :

> un paquet de transactions envoyé ensemble

Dans Jawas, le cas fréquent est souvent plus simple que l'image mentale "gros paquet complexe".

En pratique, Jawas envoie souvent un bundle contenant une transaction principale de liquidation.

L'intérêt du mot bundle n'est donc pas seulement "plusieurs transactions".
L'intérêt est aussi :

- un mode de soumission spécifique
- un chemin d'envoi spécifique
- une logique de priorité associée au tip

---

## 5. C'est quoi le tip Jito ?

Le **tip** est un paiement de priorité ajouté pour rendre le bundle plus compétitif.

Dans Jawas, tu vois des logs du type :

- `tip=100000`
- `tip_account=...`

Il faut lire cela comme :

- `tip` = montant payé pour la priorité
- `tip_account` = compte receveur du tip dans le circuit Jito

La logique économique est simple :

> plus la concurrence est forte, plus il peut être nécessaire de payer pour être compétitif

Attention :

> payer un tip ne garantit pas la victoire

Cela améliore simplement la compétitivité potentielle de l'envoi.

---

## 6. Où Jito se place dans le pipeline de Jawas

Le pipeline simplifié de Jawas ressemble à ceci :

1. détection de l'opportunité
2. résolution du contexte Kamino
3. construction des instructions
4. ajout du compute budget
5. ajout du tip
6. signature de la transaction
7. envoi via Jito

Donc Jito n'est pas le moteur de détection.
Jito n'est pas non plus la logique Kamino elle-même.

Jito intervient au moment :

> **où Jawas a déjà décidé "je veux tirer maintenant"**

Le passage du code où le tir est construit est ici :

- [src/application/hunter.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/application/hunter.rs:2920)

L'adaptateur HTTP/JSON-RPC Jito est ici :

- [src/infrastructure/jito.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/infrastructure/jito.rs:1)

---

## 7. Ce que Jawas appelle exactement

Dans Jawas, l'adaptateur Jito fait un appel JSON-RPC à la méthode :

- `sendBundle`

Le principe est :

1. la transaction Solana est sérialisée
2. elle est encodée en base64
3. elle est envoyée à l'endpoint Jito

Le code de référence est :

- [src/infrastructure/jito.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/infrastructure/jito.rs:24)

Le corps de requête est conceptuellement :

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "sendBundle",
  "params": [
    ["transaction_base64"],
    { "encoding": "base64" }
  ]
}
```

Il faut bien comprendre que :

> côté Jawas, Jito est utilisé comme une API JSON-RPC spécialisée d'envoi de bundle

---

## 8. Ce que renvoie Jito

Quand `sendBundle` réussit côté API, Jito renvoie un identifiant de bundle.

Dans Jawas, cela devient :

- une ligne `bundle_sent`
- avec un `bundle_id`

Cela signifie :

> **l'endpoint Jito a accepté la requête de soumission**

Mais cela ne signifie pas automatiquement :

- inclusion on-chain
- succès final de la liquidation
- victoire contre les autres bots

La distinction est très importante.

Donc la lecture correcte est :

- `firing` = Jawas a décidé de tirer
- `bundle_sent` = l'API Jito a accepté la soumission
- résultat final = autre sujet

---

## 9. Pourquoi `bundle_sent` n'est pas une preuve de victoire

Beaucoup de confusion vient de là.

`bundle_sent` ne veut pas dire :

- "la liquidation a eu lieu grâce à nous"
- "notre transaction a été incluse"
- "on a gagné la course"

Cela veut seulement dire :

> **Jawas a réussi à faire accepter sa requête par le block engine Jito**

Il reste ensuite d'autres couches entre :

- acceptation API
- inclusion réelle
- exécution dans le bon bloc
- victoire économique sur la concurrence

Donc Jito améliore le chemin d'envoi.
Il ne supprime pas toute l'incertitude.

---

## 10. Le block engine, en termes simples

Tu n'as pas besoin ici d'une définition ultra académique.

Pour Jawas, tu peux retenir ceci :

> le block engine Jito est l'infrastructure qui reçoit les bundles et les traite dans un circuit orienté priorité / compétition

L'image mentale utile n'est pas :

> "un nœud RPC de plus"

L'image mentale utile est plutôt :

> "un point d'entrée spécialisé pour soumettre des transactions de manière compétitive"

---

## 11. Pourquoi ne pas envoyer seulement via RPC standard ?

Parce qu'un bot de liquidation n'est pas une application tranquille.

Il agit dans un environnement où :

- plusieurs acteurs voient des opportunités proches
- les marges temporelles sont petites
- la priorité économique compte

Un RPC standard est suffisant pour :

- lire des comptes
- simuler
- demander des transactions

Mais pour la compétition d'envoi, Jito apporte une mécanique plus adaptée au contexte MEV / priorité.

Dans Jawas, cela revient à dire :

> HTTP RPC standard sert surtout à lire et préparer
>
> Jito sert surtout à pousser le tir

---

## 12. La place du `tip_account`

Dans le code, Jawas sélectionne un compte de tip.

Référence :

- [src/application/hunter.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/application/hunter.rs:2923)

L'idée simple est :

- le tip n'est pas juste un nombre abstrait
- il doit être dirigé vers un compte prévu pour cette mécanique

Tu peux voir le `tip_account` comme :

> la destination opérationnelle du paiement de priorité utilisé dans le bundle

---

## 13. La recommandation de tip

Jawas sait aussi appeler une méthode Jito pour obtenir une recommandation de tip :

- `getTipFloor`

Le code associé est :

- [src/infrastructure/jito.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/infrastructure/jito.rs:64)

La réponse contient notamment une statistique comme :

- `landed_tips_50th_percentile`

L'idée pédagogique est simple :

> Jito peut fournir une indication du niveau de tip observé sur les bundles qui landent

Ce n'est pas un oracle parfait.
Mais c'est une aide pour calibrer la compétitivité.

---

## 14. Le retry dans Jawas

Dans Jawas, un tir Jito n'est pas forcément limité à un seul appel.

Le runtime peut autoriser plusieurs tentatives selon la configuration.

Dans les logs, cela apparaît via :

- `max_send_attempts`
- `attempt=1/2`
- `attempt=2/2`

L'idée est :

1. on tente un premier envoi
2. si l'échec semble retryable, on peut retenter
3. le tip peut être ajusté au retry

Cette mécanique n'explique pas ce qu'est Jito en soi, mais elle explique comment Jawas l'utilise concrètement.

---

## 15. Le `rate gate` local de Jawas

Il faut distinguer deux choses :

- **Jito** comme infrastructure externe
- **le `rate gate`** comme règle locale de Jawas autour de cette infrastructure

Le `rate gate` est du code Jawas, pas une propriété fondamentale de Jito.

Référence :

- [src/application/hunter.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/application/hunter.rs:166)

Son rôle est simple :

> éviter que Jawas ne spamme `sendBundle` trop vite

Règles principales :

- un seul envoi Jito en cours à la fois
- un intervalle minimum configurable entre deux envois
- un budget d'attente maximum

Donc, si tu veux bien comprendre la note :

- Jito = le système externe d'envoi
- `rate gate` = la police locale que Jawas applique avant d'appeler Jito

---

## 16. Les erreurs possibles côté Jito

Quand Jawas parle à Jito, plusieurs choses peuvent se passer.

### Succès API

La requête est acceptée :

- `bundle_sent`

### Erreur API

Jito renvoie un objet d'erreur.

Dans le code, cela remonte sous la forme :

- `Jito error: ...`

### Réponse invalide

La structure de réponse n'est pas celle attendue.

### Contexte réseau / provider

Jawas peut aussi voir des messages du type :

- congestion réseau
- rate limit

Ces cas-là appartiennent à la communication avec l'infrastructure Jito, pas à la logique Kamino elle-même.

---

## 17. Ce que Jito change conceptuellement pour un bot

Sans Jito, la vision naïve est :

> "j'ai une transaction, je l'envoie"

Avec Jito, la vision devient :

> "j'ai une transaction, je l'envoie dans un circuit où la priorité économique et la qualité de soumission sont explicitement importantes"

Pour un bot de liquidation, c'est un changement de mentalité important.

On ne pense plus seulement :

- validité de la transaction

On pense aussi :

- qualité de transmission
- compétitivité de l'envoi
- agressivité du tip

---

## 18. Ce que Jito ne fait pas

Il faut aussi comprendre les limites.

Jito ne fait pas à lui seul :

- la détection des opportunités
- la résolution des comptes Kamino
- la construction correcte de la liquidation
- la preuve que l'on a gagné

En d'autres termes :

> Jito améliore un maillon très précis
>
> il ne remplace pas tout le pipeline

---

## 19. La phrase de synthèse à retenir

Si tu devais garder une seule phrase :

> **Dans Jawas, Jito est le mécanisme de soumission prioritaire des transactions de liquidation via `sendBundle` et tip.**

Et si tu devais en garder deux :

> **`bundle_sent` veut dire "soumis avec succès à l'API Jito".**
>
> **Cela ne veut pas dire "liquidation gagnée on-chain".**
