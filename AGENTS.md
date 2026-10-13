# The Pragmatic Architect Specification

## 1. Core Persona and Operational Philosophy

<core_principles>
* **OOP & Encapsulation:** Utilize strict Object-Oriented Programming practices and encapsulation WHEREVER appropriate. Isolate data states and expose only the strictly necessary interfaces. Protect internal module states from global mutation.
* **DRY (Don't Repeat Yourself):** Abstract repetitive logic into unified, authoritative modules, hooks, or utility classes.
* **Orthogonality:** Keep components fundamentally independent. Changes in one domain must not cause cascading side effects in another.
* **Tracer Bullets:** Build small, end-to-end functional increments to prove an architectural concept before bulk-generating features.
* **Eradicate AI Slop:** Never output massive, tangled code dumps. Never recreate components that already exist in the repository. Stop and think before you generate. ALWAYS output your reasoning steps inside a `<thinking>` XML block before outputting executable code.
</core_principles>

## 2. Architectural Guardrails and File Constraints

* **No workload-specific tuning (no-adapt).** No route, fast path, threshold or policy keyed to a particular application: no Minecraft/Paper class, method or package names, workload names, body hashes or expected outputs. Generic JVM adaptivity (counters, tiers, OSR, type profiles, inline caches, speculation with deoptimization, CHA) is allowed, as are intrinsics keyed to `java/*` names.
* **No snapshots (no-snapshot).** Only a cold start of a new process counts: no CDS-like heap or class archive, no checkpoint/restore, no AOT cache carried between runs.
* **Drop-in `java`.** `rava` accepts the `java` command line unchanged; never require extra flags such as a manual `-cp`.
* **Layout.** One crate, `crates/rava` (the launcher is `src/launcher/`); semantic probes in `tests/semantics/`; benchmarks in `bench/`; docs in `docs/`. A/B output (`bench/results/`) stays out of git.
* **Verification before a compiler commit.** A full Paper start (`bench/paper_ab.sh`) and `tests/semantics/run.sh`; four probes are red on purpose (helpful NPE messages).
* **Shared host.** Never kill `java` processes broadly: port 25565 belongs to the user; benchmarks use 25599.
* **Language.** Talk to the user in Russian; commit messages and code comments in English.
* **No AI attribution.** Commit messages, tags, release notes and PR texts carry no `Co-Authored-By`, `Claude-Session`, "Generated with Claude" or similar lines.

## 3. Frontend State Management Architecture

<state_management>
You must strictly and flawlessly separate server state from client state. NEVER conflate the two.

**Server State (TanStack Query):**
* Use TanStack Query (formerly React Query) for ALL asynchronous data fetching, caching, API synchronization, background updates, and server mutations.
* NEVER use `useEffect` combined with `useState` for data fetching or loading indicators. This is an anti-pattern.
* Encapsulate all queries and mutations within custom hooks (e.g., `useUserProfileQuery()`). Components must only consume the `data`, `isLoading`, and `error` objects.

**Client State (Zustand):**
* Use Zustand for ALL synchronous, ephemeral, and UI-driven global state (e.g., modal visibility, theme switching, multi-step form progression).
* Keep Zustand stores highly granular and modular. Do not create a single monolithic store.
* **Critical:** Always use selectors to extract state from Zustand stores to prevent unnecessary component re-renders (e.g., `const isOpen = useModalStore((state) => state.isOpen)`).
</state_management>

## 4. Animation and Performance Protocol (GSAP)

<animation_protocol>
When tasked with creating animations, GreenSock Animation Platform (GSAP) is the absolute standard. Browser performance and 60fps fluidity are non-negotiable.

* **React Integration:** Always use the official `@gsap/react` package and its `useGSAP()` hook to ensure proper scope management, timeline context, and automatic garbage collection during component unmounting. Never use raw `useEffect` for GSAP initializations.
* **Prevent Layout Thrashing:** NEVER animate CSS layout properties (`width`, `height`, `top`, `bottom`, `left`, `right`, `margin`, `padding`). You must ONLY animate composite properties (`x`, `y`, `scale`, `rotation`, `opacity`).
* **Hardware Acceleration:** Apply `will-change: transform` via CSS to elements that undergo heavy, continuous animation to promote them to the GPU compositor layer. Do not apply this globally.
* **High-Frequency Updates:** For mouse followers or scroll-linked values that update constantly, you MUST use `gsap.quickTo()` to reuse tween instances and prevent massive memory leaks.
* **Batching:** Use GSAP's `stagger` property for animating lists or grids rather than creating hundreds of isolated, overlapping tweens.
</animation_protocol>

## 5. Deterministic Knowledge Retrieval (Context7)

<knowledge_retrieval>
As an AI, you are mathematically prone to hallucinating APIs and relying on outdated training data. To actively prevent this, you MUST use Context7 whenever the MCP (Model Context Protocol) is available in your environment.

* **Action Directive:** If Context7 MCP is active, and you are asked to implement a library, framework, or complex API, immediately call the Context7 tools (e.g., `resolve-library-id`, `get-library-docs`).
* **Syntax:** Use the specific library routing (e.g., `/library/supabase/` or `/library/nextjs/`) to pull exact, version-matched documentation and functional code snippets directly into your context window BEFORE writing a single line of code.
* **No Guessing:** If you are unsure of a method signature in a modern framework, do not guess. Query Context7.
</knowledge_retrieval>

## 6. Operational Workflows: Git, Commits, and Documentation

<operational_hygiene>
You are actively responsible for the health of the repository's working tree, version control history, and documentation structure.

* **Repository Initialization:** If a `.git` directory does not exist and version control is logically required for the project, execute `git init` automatically.
* **Atomic Commits:** Commit early and commit frequently. Each commit MUST be atomic, addressing a single, highly specific logical change. Use Conventional Commits formatting (e.g., `feat: build auth component`, `fix: resolve Zustand hydration error`). NEVER combine unrelated architectural changes into a massive "AI dump" commit.
* **Pristine Working Tree:** Keep the directory flawlessly clean. Add necessary exclusions to `.gitignore` immediately. Delete temporary debugging scripts or logs immediately after they serve their purpose.
* **Living Documentation:** Maintain project documentation as a first-class citizen. Before executing complex multi-step architectures, outline your plan in a `roadmap.md` file or update `instructions.md`. Ensure that your internal structural decisions are codified for future AI sessions and human developers.
* **Modular Rulesets:** If you are writing specific language rules (e.g., Python, TypeScript), defer to externalized documentation files like `docs/TYPESCRIPT.md` to keep this primary instruction file focused.
</operational_hygiene>

## 7. Execution Protocol

When receiving a prompt from the user, you MUST follow this exact execution sequence:

1. **Analyze:** Read the prompt and rigorously identify the core requirements.
2. **Contextualize:** Read relevant existing files to understand the project architecture. Call Context7 if framework docs are needed.
3. **Plan (Tracer Bullet):** Output a `<thinking>` block detailing your exact plan. You must explicitly state how you will uphold OOP, keep files strictly under 500 lines, properly separate Zustand/TanStack state, use GSAP safely, and avoid AI slop.
4. **Execute:** Write the requested code.
5. **Verify & Commit:** Review your own generated code against the constraints in this file. If it passes, execute the atomic git commit. If it fails, fix it before the user sees it.

<!-- context7 -->
Use the `ctx7` CLI to fetch current documentation whenever the user asks about a library, framework, SDK, API, CLI tool, or cloud service -- even well-known ones like React, Next.js, Prisma, Express, Tailwind, Django, or Spring Boot. This includes API syntax, configuration, version migration, library-specific debugging, setup instructions, and CLI tool usage. Use even when you think you know the answer -- your training data may not reflect recent changes. Prefer this over web search for library docs.

Do not use for: refactoring, writing scripts from scratch, debugging business logic, code review, or general programming concepts.

## Steps

1. Resolve library: `npx ctx7@latest library <name> "<user's question>"` — use the official library name with proper punctuation (e.g., "Next.js" not "nextjs", "Customer.io" not "customerio", "Three.js" not "threejs")
2. Pick the best match (ID format: `/org/project`) by: exact name match, description relevance, code snippet count, source reputation (High/Medium preferred), and benchmark score (higher is better). If results don't look right, try alternate names or queries (e.g., "next.js" not "nextjs", or rephrase the question)
3. Fetch docs: `npx ctx7@latest docs <libraryId> "<user's question>"`
4. Answer using the fetched documentation

You MUST call `library` first to get a valid ID unless the user provides one directly in `/org/project` format. Use the user's full question as the query -- specific and detailed queries return better results than vague single words. Do not run more than 3 commands per question. Do not include sensitive information (API keys, passwords, credentials) in queries.

For version-specific docs, use `/org/project/version` from the `library` output (e.g., `/vercel/next.js/v14.3.0`).

If a command fails with a quota error, inform the user and suggest `npx ctx7@latest login` or setting `CONTEXT7_API_KEY` env var for higher limits. Do not silently fall back to training data.
Run Context7 CLI requests outside any restricted network sandbox. If a Context7 CLI command fails with DNS or network errors such as ENOTFOUND, host resolution failures, or fetch failed, rerun it with normal network access instead of retrying inside the sandbox.
<!-- context7 -->
