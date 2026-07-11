# Fork acknowledgement

`d2b-clip-picker` is forked from
[`Sirulex/cursor-clip`](https://github.com/Sirulex/cursor-clip), originally by
Sirulex, at upstream commit
`7e12054e55b7b2c34eff8638b88488403686e8dd`.

The project retains the upstream GPL-3.0-only license and the compact GTK
overlay interaction model. The fork replaces the standalone clipboard-manager
role with d2b's split design: `d2b-clipd` owns clipboard state, policy, and
transfer fulfillment, while this repository is a one-request presentation
client that returns only `Select` or `Cancel`.

Upstream remains the source of the original interaction concept. Security or
protocol behavior specific to d2b should be reported to this fork rather than
to the upstream project.
