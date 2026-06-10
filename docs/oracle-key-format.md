# Oracle Key Format Reference

Oracle keys must fit within the Soroban `Symbol` type — maximum 9 alphanumeric characters.

## Key Encoding Convention

```
<type><location><period>
```

| Component  | Chars | Example           |
|------------|-------|-------------------|
| type       | 1–2   | `r` (rainfall)    |
| location   | 3–5   | `kis` (Kisumu)    |
| period     | 4     | `2606` (Jun 2026) |

## Registered Keys

### Weather

| Key       | Description                            | Unit       |
|-----------|----------------------------------------|------------|
| `kis2606` | Kisumu rainfall, June 2026             | mm × 10⁷   |
| `nbi2606` | Nairobi temperature, June 2026         | °C × 10⁷   |
| `msa2607` | Mombasa wind speed, July 2026          | km/h × 10⁷ |

### Flight

| Key       | Description                    | Unit              |
|-----------|--------------------------------|-------------------|
| `kq1002606` | Kenya Airways KQ100 Jun 2026 | delay_min × 10⁷   |
| `et3002606` | Ethiopian Airlines Jun 2026  | delay_min × 10⁷   |

### On-chain (DeFi)

| Key       | Description                        | Unit              |
|-----------|------------------------------------|-------------------|
| `aavetvl` | Aave protocol TVL drop %           | % × 10⁷ (0-100)  |
| `comptvl` | Compound protocol TVL drop %       | % × 10⁷ (0-100)  |

### Disaster

| Key       | Description              | Unit                   |
|-----------|--------------------------|------------------------|
| `nbi2606` | Nairobi seismic data     | Richter × 10⁷          |
| `msa2606` | Mombasa flood level      | meters × 10⁷           |

## Adding a New Key

1. Choose a 9-char max Symbol that encodes type + location + period.
2. Register a corresponding oracle node with `add_oracle`.
3. Document it in this file and in `ARCHITECTURE.md § Oracle Key Table`.
4. Update the frontend `src/lib/constants.ts` oracle key registry.
