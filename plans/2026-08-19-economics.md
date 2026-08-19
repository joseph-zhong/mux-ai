# Do the economics of bring-your-own-GPU make sense?

Companion to `plans/2026-08-19-stock-take.md`, which defers direction (c) —
bring-your-own-GPU for choose-your-own-model — on economic grounds. This is the
argument for that deferral, and the answer to the sharper question behind it:

> For GPU providers it seems like an insurmountable advantage to
> bring-your-own-compute-and-further-improve-your-own-model.

Short answer: **the feedback loop is real and is probably decisive, but it does not
accrue to GPU providers.** It accrues to whoever sees the trajectories. Those are
different companies, and confusing them leads to building the wrong thing.

> All dollar figures below are order-of-magnitude planning estimates, not quoted
> prices. They are here to establish which side of a 5x line something falls on, and
> the conclusions only depend on that.

## 1. The unit economics for one developer

Three ways to get tokens into an agent loop:

| | Rent a GPU | Own the hardware | API |
|---|---|---|---|
| Cost shape | ~$2–3/hr for an H100-class card | large one-time, ~zero marginal | per-token |
| Monthly at full-time | ~$1,500–2,200 | amortised | usage-dependent |
| Idle cost | **full price** | electricity | **zero** |
| Batching across tenants | no | no | yes |
| Capability ceiling | best open weights | fits-in-memory open weights | frontier |

The decisive column is **idle cost**, and the decisive number is duty cycle.

One developer running agents does not produce continuous load. Even running several
sessions in parallel — which is mux-ai's entire premise — the aggregate is bursty:
minutes of generation, then a human reads a diff, then a build runs, then someone
goes to lunch. A generous estimate for a heavy user is 10–20% duty cycle against a
dedicated card.

That is the whole argument. A rented GPU is billed at 100% and used at 15%, so the
effective cost per useful token is roughly **5–7x** the sticker rate. An API provider
runs the same silicon near saturation by batching thousands of tenants whose bursts
interleave, and passes some of that back. Multi-tenancy is not a business model
detail; it is the physics of the cost structure. A single tenant cannot replicate it,
at any level of engineering skill.

Local inference on unified-memory Apple hardware is the interesting exception, because
it removes idle cost entirely — the machine is already bought and already on. That
makes the marginal token nearly free, and it is why local-first is genuinely
attractive for high-volume, low-stakes work. What it costs instead is capability: the
models that fit are not the models that make agentic coding work, and agentic coding is
unusually unforgiving of capability gaps, because errors compound across a long
trajectory instead of being caught in a single response.

**Conclusion for one developer:** renting is worse than API on cost and worse on
capability. Owning is better on marginal cost and worse on capability. Neither wins on
economics alone. Both win on things that are not economics — data never leaving the
machine, no rate limits, fixed and predictable spend, the ability to fine-tune. Those
are real reasons, and they are the reasons to support BYO-GPU eventually. They are not
reasons to build it now.

## 2. Where the feedback loop actually is

The premise deserves to be taken apart, because it contains two different businesses:

**Renting GPUs is a commodity.** The product is undifferentiated FLOPs. Customers
switch on price and availability. Critically, a bare-metal renter running the
customer's own weights **does not see the customer's data** — that is most of the
reason to rent bare metal. No data means no feedback loop. Renters compete on capital
cost and utilisation, which is a fine business and a completely different one.

**Serving a model you trained is where the loop closes.** The provider sees every
prompt, every completion, and — this is the part that matters for coding — every
*outcome*. Coding agents emit an unusually clean reward signal: the tests passed or
they did not, the build compiled or it did not, the human accepted the diff or reverted
it. That is verifiable reward at scale, which is the scarcest input in post-training
right now. Most domains have to pay humans to produce preference data of far worse
quality.

So the loop is: serve agents → observe trajectories with automatic ground-truth labels
→ train on what worked → serve a better agent → attract more agent usage. That does
compound, and it plausibly compounds faster than anything a pure infrastructure player
can build, because it is not gated on capital.

**Is it insurmountable?** It is a strong advantage, with three real limits worth
naming:

1. **It is domain-specific.** The loop is strong exactly where rewards are verifiable.
   That is a large, valuable domain — code — but it is not general capability.
2. **Trajectory data has a short half-life.** It is tied to a model's own failure
   modes, its tool schemas, and the libraries in use. Last year's corpus of "how the
   model got stuck" is worth much less against this year's model. It is a flywheel with
   friction, not an annuity.
3. **The moat is at the model layer, so it does not extend downward.** Owning the loop
   makes you the best model. It does not make you the best terminal multiplexer, or the
   best sandbox, or the best cost router — and it gives you no particular reason to be.

## 3. What this means for mux-ai

**mux-ai is not in the loop, should not try to be, and does not need to be.**

The economics of this project are not the economics of inference. mux-ai has no
marginal cost: no daemon, no server, no hosted anything. `ARCHITECTURE.md`'s "no
daemon" is an economic property as much as a design one. It does not need a business
model to be worth its own maintenance; it needs to keep costing nothing to run.

That reframes the question. It is not *"can mux-ai win against providers who own the
loop"* — it cannot and is not playing. It is *"what is worth building in a world where
model capability is someone else's compounding asset?"* Three answers:

**Model-agnosticism as a hedge, not a moat.** Being able to point a session at any
backend has no defensive value — everyone can do it. It has real *option* value: when
the capability gap narrows, or one provider's pricing moves, switching is a config line
instead of a rewrite. That is exactly why the stock-take puts the agent abstraction
first and BYO-GPU nowhere. Build the seam; skip the substance until the substance pays.

**Orchestration and accounting are not commoditised by better models.** Better models
make agents *more* parallel, not less, which makes "what are my twelve agents doing,
what are they costing me, which worktree is 4GB of stale `target/`" a bigger problem
rather than a smaller one. `status`/`reset` already exist because worktrees are cheap
until build tooling touches them. Capability improvements do not touch that.

**Cost-aware routing is the one place the economics above become a feature.** Section 1
concludes that no single backend wins: local is free-marginal but weak, API is capable
but metered, rented is neither. That is precisely the shape of problem a router solves
— cheap local model for the mechanical 80% (renames, test scaffolding, lint fixes),
frontier API for the 20% that actually needs reasoning. mux-ai already sits at the
right altitude to make that call, because it is the thing that knows a session exists,
what it is doing, and what it has cost. It cannot make that call today only because a
session's agent is an opaque command string — weakness #2 in the stock-take, and the
same 50 lines that everything else here waits on.

## 4. Answer

**Do the economics make sense?** For BYO-GPU as a product direction, no — not for a
single developer, not at current prices, not at realistic duty cycles. Defer it.

**Is the provider feedback loop insurmountable?** For the model layer, close to it, and
worth respecting rather than fighting. But it belongs to model-and-serving providers,
not to GPU providers, and it does not extend to the orchestration layer where this
project lives.

**So what should this project do?** Stay cheap, stay local, stay model-agnostic, and
build the one abstraction that keeps every option open. The compounding advantage in
models is real; the correct response to it is to be the layer that can switch, account,
and route — not the layer that tries to compete on capability it cannot fund.
