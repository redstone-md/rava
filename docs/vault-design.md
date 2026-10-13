# RAVA Vault защищённые ядра модов

## Назначение и граница защиты

RAVA Vault — проект функции protected modules для авторов модов и модпаков. Она разделяет открытый loader bridge и закрытое вычислительное ядро. В рекомендуемом режиме закрытое ядро исполняется на сервисе автора и никогда не передаётся компьютеру игрока или недоверенного владельца Minecraft server.

Из дампа клиента нельзя получить реализацию, которую клиент не получал. При этом клиент получает результаты и может анализировать поведение, записывать ответы, делиться разрешённой сессией либо пытаться восстановить похожий алгоритм. Защита confidentiality реализации не равна запрету любых утечек и имитации поведения.

Полностью локальное автономное исполнение на обычном компьютере, где получатель контролирует OS, VM, native libraries и debugger, не даёт гарантии невозможности извлечения. Изменённая сборка открытой RAVA может записать plaintext в точке загрузки, IR, JIT code или данные во время исполнения. Удаление исходного bytecode и ключа после компиляции не удаляет исполняемую реализацию и её поведение.

Сам Java также способен использовать серверное исполнение и hardware isolation. Отличие продукта RAVA — встроенный module lifecycle, SDK для loader bridge, подписанные contracts, управление лицензиями и инструмент публикации. У Rust или особого bytecode format нет исключительного свойства физически невозможного копирования.

## Режимы

| Режим | Где ядро и ключи | От чего защищает | Условия и ограничения |
| --- | --- | --- | --- |
| Remote core, рекомендуемый MVP | Только сервис автора или доверенный worker | Дамп клиента, модлоадер и изменённая клиентская VM не содержат core | Требуется сеть; backend availability и авторизованные ответы остаются частью продукта |
| Attested worker, следующий этап | Изолированный worker внутри hardware TEE, ключ выдаётся после проверки attestation | Может защитить core от недоверенного hosting administrator в пределах модели hardware | Требуются поддерживаемое железо, проверяемые measurements, обновления, отсутствие guest admin/debug доступа у получателя и isolation от произвольных mods |
| Local licensed container | Encrypted package на клиенте, plaintext или машинный код возникает локально | Ограничивает случайное копирование и повышает цену анализа | Root/modified RAVA/native injection остаются способами извлечения; не отвечает требованию абсолютной confidentiality |

Для моделей, текстур, звука и иной информации, которую необходимо отдать renderer или показать игроку, полноценный запрет копирования не обеспечивается. В Vault можно держать генератор/правила на backend; опубликованный результат всё равно доступен получателю.

Hardware attestation является проверкой evidence по заданной policy. Она не означает автоматическую безопасность приложения и не устраняет все side channels. SEV-SNP защищает guest от определённых host/hypervisor угроз; если недоверенный получатель управляет самим guest или выполняет произвольный native code в защищённом worker, граница защиты нарушена.

## Что считается ценным ядром

Подходящие задачи: proprietary generation rules, author-owned procedural algorithms, серверная проверка лицензий, сложные batch decisions, экономические и progression rules, которые можно применять по сериализованному input. Оптимальный контракт возвращает конечный результат или ограниченный effect plan.

Rendering, каждое чтение block state, каждый predicate в entity tick и операции, которым нужна свободная работа с Minecraft heap, не подходят для удалённого вызова без изменения архитектуры. Ценное ядро должно быть выделено автором явно. Автоматического превращения произвольного existing mod jar в недампимый объект нет.

## Компоненты

1. **Publisher tool** собирает public bridge и private core отдельно, фиксирует API contract и version digest, подписывает manifest. Private core не входит в bridge jar.
2. **Loader bridge SDK** имеет адаптеры Forge, Fabric и NeoForge. Он участвует в обычной регистрации модов, events и Mixins, преобразует inputs в ограниченные immutable messages.
3. **RAVA module manager** проверяет public manifest, организует sessions, typed serialization, quotas, async batches и применение results. Его интерфейс program-agnostic; Minecraft-specific integration живёт в SDK/mod, не в policy VM.
4. **Entitlement service** проверяет право account/server instance использовать module/version и выдаёт короткую session capability. Решение принимает backend.
5. **Execution worker** владеет private implementation и private state. Worker не загружает произвольные клиентские mods, plugins, JNI или patches.
6. **Result applier** на стороне Minecraft проверяет epoch/world version/entity handles и применяет effect plan в детерминированном порядке на owner thread.

RAVA module manager нельзя считать надёжным verifier на машине под контролем получателя. Клиентские подписи/проверки помогают поддерживаемому клиенту, а security decisions об авторизации и выдаче секретов остаются на backend или hardware-backed verifier.

## Контейнер и подпись

Предлагаемое расширение — .rvault. Это новый формат и API, которых пока нет в текущем репозитории.

Public manifest содержит formatVersion, publisherId, moduleId, moduleVersion, executionMode, apiDigest, publicBridgeDigest, loaderCompatibility, exports, input/output size limits, capabilities, allowed protocol version и signingKeyId.

Для remote mode публичный контейнер содержит manifest и bridge metadata, а private payload хранится на worker. Это исключает ненужную доставку ciphertext/ключей на клиент.

Publisher подписывает canonical manifest и digests payloads, а не произвольную JSON serialization. Signature отвечает за origin/integrity; encryption отвечает за confidentiality. Применять готовые проверенные реализации Ed25519 и AEAD, без собственного шифра. Key rotation и key revocation входят в publisher trust store.

Шифрование payloads для storage/attested worker должно связывать publisher/module/version/apiDigest через authenticated additional data. Для каждого ciphertext нужна корректная уникальность nonce относительно ключа. Ключи private core не входят в bridge, diagnostic output, environment dumps и клиентские capability tokens.

## Предлагаемый API

API намеренно работает с typed values и opaque handles, без Java object references:

    ProtectedModules.open(moduleId, apiDigest, entitlement)
    ModuleSession.submitBatch(worldEpoch, requestSequence, deadline, inputs)
    CompletionStage<ResultBatch>
    ResultBatch.effects
    ModuleSession.close()

Это форма будущего API, не существующие методы RAVA. SDK может генерировать typed interface из contract, чтобы mod author не работал с сырыми payloads.

ResultBatch включает module/version/apiDigest, session epoch, request sequence, input digest, ordered result records и status. Client проверяет freshness/world identity перед application. Immutable input snapshots создаются на owner thread; worker не получает указатель на Java heap.

Ошибки имеют явные outcomes: Denied, Expired, IncompatibleContract, StaleWorld, DeadlineExceeded, ServiceUnavailable, ResourceLimit, WorkerFailure. Они не превращаются автоматически в отключение проверки лицензии или выдачу private core.

## Session lifecycle

1. Public bridge регистрируется стандартным loader. Module manager проверяет signed manifest и соответствие API/bridge digests.
2. Backend аутентифицирует account/server instance и проверяет entitlement. Лицензия проверяется на сервере для каждого авторизационно значимого действия.
3. Backend открывает capability с ограничением module/version/exports, session epoch, expiry, quota и session/channel binding. Public build hash, присланный обычным клиентом, не является attestation.
4. Client передаёт bounded batch с sequence number, epoch и deadline через authenticated transport.
5. Worker валидирует contract, capability, limits, replay policy и inputs, выполняет private code и возвращает bounded result. Core bytes никогда не отправляются в ответе.
6. Result applier отвергает устаревшие результаты и применяет валидные effects на owner thread в определённом порядке.
7. Renewal/expiry/revocation закрывает доступ на backend. In-flight requests имеют определённый исход; world save/checkpoint не уничтожается из-за недоступности сервиса.

Для повторов idempotent operation backend связывает result с request ID и input digest. Повтор с другим input под тем же ID отвергается. Для stateful actions обработка sequence/epoch исключает двойное применение. Local clock не считается источником истины для entitlement expiry.

Сессию, которой полностью управляет adversarial client, можно relay-ить или использовать как oracle. Quotas, limited exports и backend state уменьшают злоупотребление, но не доказывают невозможность передачи доступа другому человеку. Hardware session keys дают дополнительные свойства только при реальной attestation защищённой среды.

## Совместимость с Forge Fabric и NeoForge

Обычный bridge jar остаётся видимым loader: metadata, event registration, public resources, Mixins и debugging работают стандартно. Private core не объявляется обычным class resource и не передаётся transformers.

Mixin применяется к class bytecode до definition; transformed classes могут экспортироваться стандартными debugging tools. Поэтому sensitive implementation, встроенная в такой target/Mixin, проходит через доступный loader pipeline. Private core выносится за его границу; публичные wrappers могут быть скопированы без раскрытия core.

В remote core и корректно изолированном attested worker клиентские Mixins, ASM, reflection и access transformers не получают private core для изменения. Они могут работать с public bridge и Minecraft classes. Это явная compatibility boundary: авторы, которым нужно смешивать чужие callbacks и objects с private internals, должны переработать этот контракт. Для local licensed container под контролем получателя такая гарантия отсутствует.

Native modules и Unsafe в Minecraft process не должны иметь доступ к private worker address space. В remote MVP это обеспечивается размещением worker вне клиентской машины. Обычный локальный child process сам по себе не создаёт нужной OS security boundary: родительский процесс или native mod под тем же OS account может иметь доступ к его памяти. Нужны отдельная identity, ограниченные process permissions и sandbox policy; от владельца OS с admin/root такая программная изоляция не защищает.

## Производительность

DRM не должен добавлять synchronous network round trip на каждую операцию горячего tick loop. Для обычного server tick 20 TPS бюджет составляет 50 ms; network jitter может съесть его независимо от эффективности JIT.

Использовать coarse-grained batches, асинхронную подготовку работы, ограниченные сообщения, pooling buffers и owner-thread application. Наиболее подходящие задачи — generation/precomputation и decisions с заранее известным сроком. Inputs должны минимизировать объём world snapshot.

Fallback задаёт автор для каждого export. Допустимы ожидание вне критического tick, отказ от нового protected action, использование ранее разрешённого result или публичная degraded functionality. Передача private implementation для автономного fallback меняет гарантию confidentiality и требует отдельного local mode.

TEE worker рядом с Minecraft host может уменьшить latency, но включает serialization, enclave/VM boundary overhead и restrictions. Ни remote mode, ни TEE не дают автоматического ускорения VM. Сравнивать RAVA/JVM performance без DRM и overhead выбранного DRM mode отдельно.

## Threat cases и ожидаемый результат

| Сценарий | Remote core |
| --- | --- |
| Дамп Minecraft/RAVA memory | Public bridge, inputs, outputs и session material доступны; private implementation отсутствует |
| Dump transformed classes из loader | Видны public wrappers; core class bytes не участвовали в transformation |
| Изменение RAVA для обхода client license check | Не выдаёт private core; backend продолжает entitlement checks |
| Перехват network traffic | Authenticated transport ограничивает внешнее чтение; локальный клиент видит собственные plaintext inputs/outputs |
| Компрометация авторизованного клиента | Возможны oracle/relay злоупотребления; ограничиваются exports/quotas, без абсолютной гарантии |
| Компрометация trusted worker/backend | Core может быть раскрыт; требуются isolation, key management и operational controls |
| Копирование визуального результата | Возможно; renderable output уже раскрыт пользователю |
| Offline на обычном PC | Remote core недоступен; заранее выбранная degraded/offline functionality сохраняет явный контракт |

Не внедрять kernel drivers, скрытое сканирование чужих процессов и destructive reactions. Это не создаёт надёжную confidentiality boundary и повышает стоимость поддержки и риск аварий.

## План реализации

**Этап 1 — proof of boundary.** Один public bridge и один private batch operation на отдельном авторском worker. Зафиксировать contract, entitlements, expiry, serialization и errors. Проверить packet/class/memory dumps клиента: private core не должен быть передан вообще. Это проверка protocol design, а не попытка доказать отсутствие любых side channels.

**Этап 2 — RAVA integration.** Generic module manager плюс SDK для одного supported loader version. Добавить quotas, ordered result applier, stale-world rejection, session cleanup и cancellation. Измерить overhead/latency under load, validate save consistency при service failure.

**Этап 3 — publisher tooling и три loaders.** Signed manifests, reproducible bridge digests, key rotation, diagnostic privacy, Forge/Fabric/NeoForge adapters, compatibility matrix и migration policy. Loader versions нужно закреплять и тестировать отдельно.

**Этап 4 — attested worker.** Выбрать hardware platform под реальные deployment requirements. Проверять fresh attestation, worker image/module/contract measurements, security patch level и debug policy; связывать выдачу secret key с этой evidence и защищённым session key. Не загружать arbitrary mods в trusted worker.

Local licensed container является отдельным дополнительным продуктовым режимом с более слабой гарантией. Он не должен выпускаться под обещанием невозможного dump.

## Критерии готовности

- Private core отсутствует в клиентском distribution, class resources, IR/JIT dumps, crash reports и network responses.
- Backend authorizes operations независимо от client-side checks; expired/revoked/wrong-module capabilities отвергаются.
- Bounded schema отсекает oversized/malformed messages; output не способен произвольно выполнять code на client.
- Replay, retries, world restart, stale epochs и partial application имеют deterministic semantics.
- Service outage и worker crash не портят world save и не приводят к скрытому раскрытию core.
- Все public bridges работают с заявленными loader/mod versions, а incompatible signatures fail clearly.
- p95/p99 latency и overhead измерены на целевых workloads; горячий tick не блокируется на непредсказуемом RPC.
- Для TEE отдельно проверены hostile-host/debug/snapshot scenarios и trust assumptions. На обычном client нельзя объявлять эти свойства автоматически.

## Основные источники

- [Fabric Mixin introduction](https://wiki.fabricmc.net/tutorial:mixin_introduction): transformation bytecode до class load.
- [Fabric Mixin exporting](https://wiki.fabricmc.net/tutorial:mixin_your_first_mixin): export transformed classes.
- [NeoForge mod files](https://docs.neoforged.net/docs/1.21.5/gettingstarted/modfiles/): public loader metadata.
- [RFC 9334](https://www.rfc-editor.org/info/rfc9334/): attester, verifier, relying party, appraisal policy и freshness.
- [AMD SEV SNP attestation](https://www.amd.com/content/dam/amd/en/documents/developer/lss-snp-attestation.pdf): guest owner measurement/policy, получение secrets после appraisal.
- [AMD SEV](https://www.amd.com/en/developer/sev.html): VM protection от host/hypervisor threats.
- [Intel SGX](https://www.intel.com/content/dam/www/central-libraries/us/en/documents/q3-intel-sgx-infographic1.pdf): hardware isolation и side-channel limits.
- [RFC 8032](https://www.rfc-editor.org/info/rfc8032/): Ed25519 и test vectors.
- [RAVA patch dump](https://github.com/redstone-md/rava/blob/fa9e35daeba0fbf802a3aaeb1ae01ebfc596e733/crates/rava/src/patch/mod.rs#L139-L165): существующая возможность dump targeted bytecode.

Это спецификация новой функции. Production implementation, infrastructure и loader adapters в текущий репозиторий не внесены.

