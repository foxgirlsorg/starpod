\# Goal: Implement a Rust + Slint Proxy Application



Build a functional, extensible desktop proxy application using \*\*Rust\*\*, \*\*Slint\*\* for the GUI, and \[`meow-rs`](https://github.com/meow-rs/meow-rs) as its proxy core.



Use \[`https://foxgirls.org/`](https://foxgirls.org/) as the primary visual and branding reference.



This is an implementation task, not a planning-only exercise. Research the existing ecosystem, establish a sound architecture, ask me about genuinely consequential architectural decisions, and implement the application once the necessary decisions are settled.



\## 1. Execution Instructions



\- Use all relevant available skills. Discover and read applicable skills before implementation, including Rust development, Slint, software architecture, asynchronous programming, networking, testing, security, and desktop application development.

\- Use as many parallel agents as the environment permits. Do not artificially limit agent utilization.

\- Delegate independent research and implementation tasks to specialized agents.

\- Assign clear responsibilities and file/module ownership to avoid conflicting edits.

\- Have agents independently investigate unfamiliar APIs, libraries, design references, and implementation options.

\- Use a coordinating agent to consolidate findings, establish architectural decisions, and review integration.

\- Require agents to communicate important findings, blockers, assumptions, and implementation status.

\- Do not delegate the same files to multiple agents without an explicit coordination strategy.

\- Verify all agent contributions before integrating them.

\- Use official documentation and actual source code as the primary references. Do not invent APIs, capabilities, configuration formats, or compatibility claims.

\- Prefer established, actively maintained dependencies over unnecessary custom implementations.

\- Do not add dependencies without understanding their purpose and maintenance status.



\## 2. Initial Research and Architectural Assessment



Before writing implementation code:



1\. Examine the existing repository, project structure, instructions, conventions, and available development environment. Preserve existing functionality where applicable.

2\. Investigate `meow-rs` thoroughly:

&#x20;  - Its architecture, public API, capabilities, limitations, and maturity.

&#x20;  - How it manages proxy instances and their lifecycle.

&#x20;  - Its runtime configuration format and configuration loading mechanism.

&#x20;  - Supported protocols, transports, routing capabilities, and DNS functionality.

&#x20;  - Asynchronous runtime requirements and integration with other Rust components.

&#x20;  - Platform compatibility and packaging requirements.

&#x20;  - Error handling, logging, and observability.

&#x20;  - Whether it is suitable for embedding directly into the application or requires a separate process.

3\. Investigate Slint's recommended architecture and integration patterns for a nontrivial Rust desktop application.

4\. Inspect `foxgirls.org` for its visual identity, color palette, typography, design language, components, and overall aesthetic. Translate these into a cohesive desktop application design rather than blindly copying the website layout.

5\. Evaluate the relevant dependencies and possible architectural alternatives.

6\. Identify technical risks and unresolved decisions before implementation.



\*\*Do not assume that the requested technologies are automatically compatible.\*\* Verify their compatibility and recommend changes if a different integration approach is technically necessary. Explain the consequences before changing fundamental technology choices.



\## 3. Core Application Architecture



The application should have a clean separation between the following responsibilities:



\### Proxy Core Integration



Use `meow-rs` as the actual proxy engine.



Implement a dedicated abstraction around the proxy core so that the GUI and application business logic do not depend directly on its internal implementation.



The integration should support:



\- Initializing the proxy engine.

\- Starting and stopping the proxy.

\- Restarting or reloading it when configuration changes.

\- Retrieving operational state and relevant runtime information.

\- Handling startup failures and runtime errors.

\- Exposing useful logs and diagnostics.

\- Validating configuration before applying it.



Avoid unnecessary abstractions. The architecture should remain maintainable without turning a relatively simple desktop application into an overengineered framework.



\### Subscription Management



The application must obtain proxy configuration from a remote subscription server.



Implement a subscription management layer responsible for:



\- Retrieving subscription data.

\- Handling authentication where applicable.

\- Parsing and validating received data.

\- Handling network failures, invalid responses, and expired subscriptions.

\- Updating subscription information.

\- Caching the last valid subscription where appropriate.

\- Handling subscription refreshes and configuration changes.

\- Keeping subscription-derived configuration separate from user-defined preferences.



Do not invent a server API or data schema. Investigate any existing contracts or implementations in the repository. If the actual server API, authentication method, or response format is unavailable, identify exactly what is required from me. Design an appropriate provider abstraction in the meantime, without pretending that a real integration has been completed.



\### Runtime Configuration Builder



This is one of the most important parts of the application.



The application must dynamically construct the runtime configuration consumed by `meow-rs` using two primary inputs:



1\. \*\*Remote subscription:\*\* servers, credentials, protocol parameters, available transports, and other information supplied by the subscription.

2\. \*\*Local user preferences:\*\* custom routing rules, DNS settings, connection preferences, exclusions, and other supported overrides.



Implement a dedicated configuration pipeline:



`Subscription + User Preferences → Normalization → Merging → Validation → Runtime Configuration → Proxy Core`



Requirements:



\- Maintain a clear separation between remote configuration and local preferences.

\- Define deterministic precedence rules when local preferences conflict with subscription data.

\- Never silently overwrite user-defined settings during subscription updates.

\- Preserve user configuration when subscriptions are refreshed.

\- Validate all generated configuration before applying it.

\- Detect invalid or unsupported combinations and provide meaningful errors.

\- Ensure that generated configurations are reproducible from their inputs.

\- Avoid modifying the currently running configuration until the replacement has been validated successfully.

\- Provide a safe way to recover from failed configuration updates.



Do not couple the configuration builder to the GUI or the proxy engine. It should be independently testable.



\### Custom Rules



Users must be able to define custom rules that influence proxy behavior.



Investigate the rule capabilities supported by `meow-rs` and design the user-facing rule model around actual engine capabilities.



Consider:



\- Domain-based rules.

\- IP and CIDR rules.

\- Domain suffixes and keyword matching, where supported.

\- Direct, proxy, and block actions.

\- Rule priorities and ordering.

\- Default/fallback routing behavior.

\- Rule validation.

\- Persistence and editing.

\- Conflicts between user rules and subscription-provided rules.



Do not promise rule types that cannot be represented by the underlying engine.



The user-facing representation should be independent of the core's raw configuration format. Implement a translation layer that converts normalized rules into the format expected by `meow-rs`.



\## 4. Desktop Application and UI



Use \*\*Slint\*\* for the GUI.



The visual design should be inspired by `foxgirls.org`, adapted for a modern desktop proxy application.



The UI should feel like a coherent product rather than a generic administration panel.



Establish reusable design tokens and components for:



\- Colors and backgrounds.

\- Typography.

\- Spacing.

\- Borders and corner radii.

\- Buttons and interactive states.

\- Cards and information panels.

\- Icons.

\- Navigation.

\- Status indicators.



Prioritize a clear visual hierarchy and a polished, consistent experience.



\### Initial Application Screens



Implement the following core screens:



\*\*Dashboard\*\*

\- Current proxy status.

\- Connection/disconnection controls.

\- Selected configuration or server information.

\- Subscription status.

\- Relevant connection information.

\- Clear error and loading states.



\*\*Servers / Subscription\*\*

\- Subscription information.

\- Available servers.

\- Server selection.

\- Subscription refresh.

\- Subscription update errors and status.



\*\*Rules\*\*

\- List existing custom rules.

\- Add, edit, and delete rules.

\- Configure rule actions and priorities.

\- Validate user input.

\- Clearly distinguish user-defined rules from subscription-provided rules where applicable.



\*\*Settings\*\*

\- General application preferences.

\- Proxy configuration preferences.

\- DNS and routing settings, where supported.

\- Subscription preferences.

\- Application behavior and persistence settings.



\*\*Logs / Diagnostics\*\*

\- Useful runtime logs.

\- Proxy startup and shutdown information.

\- Configuration validation failures.

\- Subscription update errors.



The exact navigation and screen layout should be determined after inspecting the visual reference and evaluating the application requirements.



\### UI Engineering Requirements



\- Keep business logic outside `.slint` UI definitions.

\- Use a clean Rust-to-Slint state and event integration pattern.

\- Ensure asynchronous operations do not freeze the UI.

\- Provide proper loading, success, failure, and empty states.

\- Avoid unnecessary UI refreshes.

\- Make state transitions predictable.

\- Do not leave interactive controls as nonfunctional placeholders.

\- Make the interface usable at different window sizes.



\## 5. Persistence and Configuration Storage



Design a persistence strategy for:



\- Application preferences.

\- User-defined rules.

\- Subscription metadata and cached subscription data.

\- Selected server.

\- Other necessary application state.



Keep secrets and sensitive information protected.



Evaluate suitable storage options based on actual requirements instead of choosing a database or serialization format arbitrarily.



The application should handle corrupted, missing, or outdated stored data gracefully.



Include a configuration versioning and migration strategy if the chosen persistence format requires one.



\## 6. Security and Reliability



Treat the application as security-sensitive networking software.



Pay particular attention to:



\- Protecting subscription credentials and proxy secrets.

\- Avoiding sensitive information in logs.

\- Validating untrusted subscription data.

\- Safe handling of remote configuration updates.

\- Preventing malformed configuration from breaking the running proxy.

\- Avoiding accidental exposure of local services or management interfaces.

\- Correct lifecycle management of background tasks and the proxy engine.

\- Graceful application and proxy shutdown.

\- Proper error propagation and recovery.



Do not introduce insecure defaults for convenience.



\## 7. Testing and Quality



Implement meaningful tests alongside the application.



Prioritize:



\- Subscription parsing and validation.

\- Configuration merging and precedence.

\- Custom rule processing.

\- Runtime configuration generation.

\- Invalid configuration handling.

\- Persistence and migrations.

\- Proxy lifecycle management.

\- Error recovery.

\- Core integration.



Use mocked subscription providers and deterministic fixtures where real server access is unavailable.



Do not make tests depend on an operational remote subscription service unless explicitly necessary.



The final application must:



\- Compile successfully.

\- Pass relevant automated tests.

\- Pass formatting and linting checks.

\- Have no obvious placeholder implementations in core functionality.

\- Handle expected failure conditions.

\- Include clear setup and development documentation.



\## 8. Architecture Decision Gate — Ask Me When Necessary



After completing the initial research, identify architectural decisions that genuinely require my input.



Do not ask questions whose answers can be established through repository inspection or documentation.



In particular, investigate whether the following decisions materially affect the architecture:



1\. \*\*Platform targets:\*\* Which operating systems should be supported, and does the underlying proxy core support them?

2\. \*\*Proxy execution model:\*\* Should `meow-rs` run embedded inside the application or as a separate managed process? Determine the technically viable options first.

3\. \*\*Subscription integration:\*\* What server API, authentication mechanism, and subscription response format should be supported?

4\. \*\*Configuration ownership:\*\* Which settings should be controlled by the subscription server and which should remain completely user-controlled?

5\. \*\*Persistence:\*\* Is a lightweight local configuration file sufficient, or is a database justified?

6\. \*\*Application lifecycle:\*\* Are system tray integration, autostart, background operation, and startup-on-login part of the initial scope?

7\. \*\*Rule management:\*\* What level of rule customization should the initial release expose?



These are examples, not a mandatory questionnaire. Only ask about choices that cannot be safely resolved from technical research.



For each question:



\- Explain the available alternatives.

\- Describe the technical trade-offs.

\- State your recommended option and why.

\- Explain how the answer affects the architecture.

\- Keep questions concise and group them into a single decision checkpoint.



Use sensible, documented defaults for decisions that do not materially affect the architecture.



\*\*Do not start implementing architecture-dependent functionality based on arbitrary assumptions.\*\* You may proceed with independent research, noncontroversial scaffolding, tests, and implementation work that does not depend on unresolved decisions.



Once I answer, incorporate the decisions into the architecture and continue implementation without repeatedly asking for approval.



\## 9. Implementation Workflow



Organize the work into the following phases:



\*\*Phase 1 — Research\*\*

\- Inspect repository and environment.

\- Load relevant skills.

\- Analyze `meow-rs`.

\- Analyze Slint integration options.

\- Inspect the design reference.

\- Evaluate dependencies and technical feasibility.

\- Delegate independent research to parallel agents.



\*\*Phase 2 — Architecture\*\*

\- Define module boundaries and responsibilities.

\- Establish application state management.

\- Define subscription and preference data models.

\- Design the configuration generation pipeline.

\- Define the proxy core adapter.

\- Establish persistence and error-handling strategies.

\- Present necessary architectural questions.



\*\*Phase 3 — Foundation\*\*

\- Set up the Rust and Slint project structure.

\- Establish dependency management.

\- Implement core application infrastructure.

\- Implement persistence and shared data models.

\- Establish logging and error handling.



\*\*Phase 4 — Functional Implementation\*\*

\- Implement subscription management.

\- Implement preference management.

\- Implement the runtime configuration builder.

\- Implement custom rules.

\- Integrate `meow-rs`.

\- Implement proxy lifecycle management.

\- Connect backend state and operations to Slint.



\*\*Phase 5 — UI\*\*

\- Implement the design system.

\- Implement the main screens.

\- Integrate real application state and operations.

\- Handle all relevant loading, error, and empty states.



\*\*Phase 6 — Verification\*\*

\- Build the application.

\- Run tests, linting, and formatting.

\- Test the complete configuration generation pipeline.

\- Verify proxy startup, shutdown, and configuration updates.

\- Fix discovered integration issues.

\- Review the implementation for architectural inconsistencies and security problems.



\*\*Phase 7 — Documentation\*\*

\- Document the architecture.

\- Explain development setup and build instructions.

\- Document subscription integration.

\- Explain configuration precedence and custom rule behavior.

\- Document platform-specific limitations.

\- Include known limitations and remaining work.



Agents may execute independent phases or tasks concurrently when dependencies permit. The phases describe logical dependencies, not a requirement for strictly sequential execution.



\## 10. Engineering Standards



\- Favor correctness and maintainability over premature optimization.

\- Use idiomatic Rust and appropriate ownership, error-handling, and concurrency patterns.

\- Avoid unnecessary `unsafe`.

\- Avoid global mutable state.

\- Avoid tightly coupled modules.

\- Avoid oversized files and monolithic application components.

\- Avoid unnecessary abstractions and excessive trait usage.

\- Keep dependencies and feature flags intentional.

\- Do not duplicate functionality already provided by `meow-rs`.

\- Do not implement fake functionality merely to make the UI appear complete.

\- Do not silently swallow errors.

\- Do not leave `TODO`, `FIXME`, `unimplemented!()`, or placeholder logic in functionality claimed as complete.

\- Clearly distinguish implemented features from mocked or unavailable integrations.



\## 11. Definition of Done



The task is complete when there is a functional Rust + Slint desktop application that:



1\. Uses `meow-rs` as its actual proxy core.

2\. Can retrieve and process subscription data through the defined integration.

3\. Combines subscription data with local user preferences.

4\. Generates and validates usable runtime configuration.

5\. Supports persistent custom routing rules within core capabilities.

6\. Can start, stop, and manage the proxy.

7\. Provides a coherent UI based on the Foxgirls visual identity.

8\. Persists application state and user preferences.

9\. Handles errors without silently corrupting configuration or losing the previous valid state.

10\. Builds and passes its relevant automated checks.



Do not declare completion merely because the GUI renders or the project compiles. The subscription-to-runtime-configuration-to-proxy execution path is the central functionality and must be verified.



Start with the research phase. Use parallel agents and all relevant available skills, then bring me the architectural decisions that genuinely need my input.

