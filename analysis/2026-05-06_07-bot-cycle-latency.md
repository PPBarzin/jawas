# Temps de cycle du bot sur les 6 et 7 mai 2026

## Objet

Cette note cherche a decomposer le **temps total** entre:

- une evolution de marche qui deplace une position
- le moment ou cette position devient effectivement liquidable
- la detection amont par le provider
- puis la reaction du bot jusqu'au `bundle_sent`

La question a trancher est:

> la latence visible vient-elle surtout du provider amont, ou du script lui-meme ?

Une partie seulement de cette chaine est mesurable directement avec les logs actuels. Le reste doit etre presente comme:

- `mesure`
- `estimation`
- `non observable aujourd'hui`

References:

- [src/application/hunter.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/application/hunter.rs:2756)
- [src/application/hunter.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/application/hunter.rs:2899)
- [src/application/hunter.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/application/hunter.rs:3637)

## Conclusion courte

Sur les vrais cas des `6` et `7 mai 2026`, le script est rapide une fois le signal recu:

- `signal recu -> firing`: `126 ms` a `271 ms`, moyenne `184.75 ms`
- `signal recu -> bundle_sent`: `298 ms` a `654 ms`, moyenne `413.5 ms`
- `signal recu -> signal_resolution_failed`: `105 ms` a `381 ms`, moyenne `175.56 ms`

La lecture la plus juste est donc:

- le script ne semble pas etre le goulet principal
- le retard dominant se joue avant `ws_received`
- le second probleme interne est la robustesse de resolution, pas la vitesse pure

En revanche, le **debut** de la chaine reste largement invisible:

- `modification du cours -> position liquidable`
- `position liquidable -> transaction de liquidation source visible`
- `transaction visible -> emission effective du signal provider`

Ces segments sont justement les candidats principaux pour expliquer le retard structurel.

## Budget de latence total

## Vue granulaire

Le cycle complet peut etre decompose ainsi:

1. `Modification du cours`
2. `Position devient liquidable`
3. `Le reseau Solana integre la transaction source qui liquidite`
4. `Le provider RPC observe la transaction / le log / la mise a jour`
5. `Le provider fait son traitement interne`
6. `Le provider pousse l'evenement vers nous`
7. `Nous recevons le signal`
8. `Resolution interne du signal`
9. `Preparation de l'opportunite`
10. `Construction / signature du bundle`
11. `Envoi du bundle`

### Statut d'observabilite et ranges par segment

| Segment | Statut | Range / temps connu | Ce qu'on sait aujourd'hui |
| --- | --- | --- | --- |
| 1. Modification du cours -> position liquidable | Non observable | `inconnu` | Depend de la pente du marche, de la marge restante et du type de mouvement. Peut etre quasi instantane ou s'etaler sur plusieurs secondes. |
| 2. Position liquidable -> tx source sur la blockchain | Non observable | `inconnu` | Segment potentiellement critique. C'est ici qu'un bot concurrent mieux place peut frapper tres vite apres le franchissement du seuil. |
| 3. tx source sur Solana -> RPC la voit | Partiellement observable | `inconnu` | La tx source existe sur la chaine, mais son `block_time` / slot n'est pas historise dans nos JSONL. |
| 4. RPC la voit -> RPC traite -> RPC envoie le signal | Non observable | `inconnu` | Couts internes provider: decoding, filtrage, fanout, scheduling du stream. Candidat majeur pour la latence amont. |
| 5. RPC envoie le signal -> nous recevons le signal | Non observable | `inconnu` | Inclut la latence reseau provider -> machine. Probablement plus petite que le reste, mais non mesuree. |
| 6. Nous recevons le signal -> echec de resolution | Mesure | `105-381 ms`, moyenne `175.56 ms` | Quand nous echouons, nous echouons vite. Le hot path ne bloque pas longtemps avant `signal_resolution_failed`. |
| 7. Nous recevons le signal -> firing | Mesure | `126-271 ms`, moyenne `184.75 ms` | Coeur du traitement bot-side avant tir. Sur les succes observes, ce segment reste sous `300 ms`. |
| 8. Firing -> bundle_sent | Mesure indirecte | `156-383 ms`, plage typique `~170-190 ms` | Derive des cas `bundle_sent`. Le cout stable visible dans `timings_ms` est surtout `send_bundle`, frequemment `172-188 ms`. |
| 9. Nous recevons le signal -> bundle_sent | Mesure | `298-654 ms`, moyenne `413.5 ms` | Meilleure mesure end-to-end bot-side disponible aujourd'hui. Le script complet reste souvent sous `0.5 s`, et le pire cas reussi ici reste sous `0.7 s`. |

## Ce qu'on peut deja dire sur la latence amont

Meme sans mesurer directement les segments `1` a `6`, on a un indice fort:

- `394` cas arrivent deja en `source_obligation_healthy`
- ces cas ont souvent `elapsed_ms = 0`

Cela signifie:

- au moment exact ou nous traitons le signal, la fenetre utile est deja fermee
- ce retard est donc **anterieur** au traitement interne du bot

Autrement dit, si le bot met environ `0.3 s` a `0.7 s` pour aller jusqu'au `bundle_sent`, mais que la position est deja redevenue saine des la reception du signal, alors le temps perdu principal est situe avant l'etape `nous recevons le signal`.

## Sankey du funnel

```mermaid
sankey
    "Signal recu (trace brute)","Obligation deja saine",394
    "Signal recu (trace brute)","Resolution echouee",36
    "Signal recu (trace brute)","Firing",4
    "Firing","Bundle envoye",4
```

Lecture:

- la plupart des signaux arrivent deja hors fenetre
- une petite partie echoue en resolution
- seulement `4` cas ont atteint le tir
- et ces `4` cas ont tous abouti a `bundle_sent`

## Decoupe bot-side mesuree

### 1. Reception du signal -> `firing`

Cas observes:

- `2026-05-06`: `142 ms`, `271 ms`, `126 ms`
- `2026-05-07`: `200 ms`

Synthese:

- min: `126 ms`
- max: `271 ms`
- moyenne: `184.75 ms`

Interpretation:

- la preparation interne avant tir reste sous `300 ms` sur les succes observes
- dans les details de trace, le bloc `prep` vaut souvent `0 ms` a `162 ms`

### 2. Reception du signal -> `bundle_sent`

Cas observes:

- `2026-05-06`: `330 ms`, `654 ms`, `298 ms`
- `2026-05-07`: `372 ms`

Synthese:

- min: `298 ms`
- max: `654 ms`
- moyenne: `413.5 ms`

Interpretation:

- le bot envoie generalement un bundle en environ `0.3` a `0.4 s`
- le cas a `654 ms` correspond a un `attempt=2/2`, donc une relance rallonge nettement le cycle

### 3. Details internes visibles dans `detail`

Sur les succes, les timings internes journalises montrent typiquement:

- `prep`: `0 ms` a `162 ms`
- `send_bundle`: `172 ms` a `188 ms`

Donc, meme sans calcul externe, la decomposition interne raconte deja une histoire claire:

- la logique de preparation coute peu
- l'envoi bundle coute environ `170-190 ms`
- le total bot-side reste inferieur a `700 ms` meme sur le cas avec retry

## Cas d'echec de resolution

Les `36` `signal_resolution_failed` de la fenetre tombent eux aussi vite apres reception:

- min: `105 ms`
- max: `381 ms`
- moyenne: `175.56 ms`

Cela veut dire:

- le bot ne "rame" pas longtemps avant de rater
- il decide rapidement qu'il ne sait pas resoudre le signal

Le probleme n'est donc pas seulement la lenteur interne. C'est surtout:

- soit un signal qui arrive trop tard
- soit un signal que le parseur / resolver ne sait pas exploiter

## Ce qu'on peut conclure, et ce qu'on ne peut pas conclure

### Ce qu'on peut conclure

- Une fois le signal recu, Jawas reagit vite.
- Le goulet principal n'est pas un temps de calcul de plusieurs secondes dans le script.
- Le probleme principal est amont du `ws_received`, ou dans la qualite du signal.

### Ce qu'on ne peut pas conclure ici

- On ne peut pas attribuer cette latence specifiquement a `Helius`, parce que ces logs sont tags `source=primary_rpc`.
- On ne peut pas non plus exclure completement un probleme de parsing interne, puisque `signal_resolution_failed` reste un vrai sous-probleme.
- On ne peut pas encore chiffrer honnetement:
  - `modification du cours -> position liquidable`
  - `position liquidable -> tx source`
  - `tx source -> emission provider`

## Formulation de travail

La phrase la plus precise a retenir est:

> Sur les 6 et 7 mai 2026, Jawas met environ `0.1` a `0.3 s` pour entrer en firing, puis environ `0.3` a `0.7 s` pour aller jusqu'au bundle. Si la liquidation est deja ratee, le retard principal se joue avant la reception du signal, pas dans le hot path interne du bot.

## Instrumentation minimale manquante

Pour rendre ce budget completement mesurable, il manque au minimum:

1. Le `slot` et le `block_time` de la tx source au moment du signal.
2. Le timestamp exact de reception provider-side si disponible.
3. Un journal du premier instant ou une obligation devient liquidable selon notre propre calcul.
4. Un tag de source plus precis que `primary_rpc` quand plusieurs providers ou modes de stream existent.

Avec ces quatre briques, on pourrait transformer cette note en vraie analyse causale end-to-end.
