# Paper recorder

This is operations practice for a future forward test: obtaining data separately, recording a hash chain, and checking alerts. It is not a hypothesis test or evidence for any hypothesis. It is not in the ledger and N remains unchanged (N = 0 for this practice). It never computes or displays performance. The recorder reads a local file and does not place orders or access a broker or network.

Each hash preimage is compact JSON with sorted keys, the `hash` field removed, and serde_json number formatting.

A gap of more than four calendar days, such as Thursday to Tuesday over a long weekend, produces an expected exit-2 alert for a human to check.

## Data rights

Tiingo's [terms of use, section 7.3](https://www.tiingo.com/tos/) state: “All data via the API is for internal consumption only.” Redistribution requires a separate permission and license. Tiingo's [API documentation](https://www.tiingo.com/documentation/general) states that Basic and Power data is for internal and personal use and may not be redistributed. Store real recorded logs only in gitignored `live/` on the server or in the private repository, subject to the applicable license; never put them in this public repository. The public repository holds code, tests, and synthetic fixtures only.
