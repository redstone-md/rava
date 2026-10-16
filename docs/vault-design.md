# RAVA Vault: защищённые ядра модов

## Назначение и граница защиты

RAVA Vault — проект функции protected modules для авторов модов и модпаков. Мод делится на открытый loader bridge и закрытое вычислительное ядро. В рекомендуемом режиме закрытое ядро исполняется на сервисе автора и не попадает ни на компьютер игрока, ни к недоверенному владельцу Minecraft server.

Реализацию, которую клиент не получал, из дампа клиента не достать. Но клиент получает результаты: он может анализировать поведение, записывать ответы, делиться разрешённой сессией или пытаться восстановить похожий алгоритм. Конфиденциальность реализации не означает, что исключены любые утечки и имитация поведения.

Полностью локальное автономное исполнение на обычном компьютере, где получатель контролирует OS, VM, native libraries и debugger, не гарантирует, что ядро нельзя извлечь. Изменённая сборка открытой RAVA может записать plaintext в момент загрузки, а также IR, JIT code или данные во время исполнения. Если удалить исходный bytecode и ключ после компиляции, исполняемая реализация и её поведение всё равно остаются.

Серверное исполнение и hardware isolation доступны и обычной Java. RAVA отличается встроенным module lifecycle, SDK для loader bridge, подписанными contracts, управлением лицензиями и инструментом публикации. Ни Rust, ни особый формат bytecode сами по себе не делают копирование физически невозможным.

## Режимы

| Режим | Где ядро и ключи | От чего защищает | Условия и ограничения |
| --- | --- | --- | --- |
| Remote core, рекомендуемый MVP | Только сервис автора или доверенный worker | Дамп клиента, модлоадер и изменённая клиентская VM не содержат core | Требуется сеть; backend availability и авторизованные ответы остаются частью продукта |
| Attested worker, следующий этап | Изолированный worker внутри hardware TEE, ключ выдаётся после проверки attestation | Может защитить core от недоверенного hosting administrator в пределах модели hardware | Требуются поддерживаемое железо, проверяемые measurements, обновления, отсутствие guest admin/debug доступа у получателя и isolation от произвольных mods |
| Local licensed container | Encrypted package на клиенте, plaintext или машинный код возникает локально | Ограничивает случайное копирование и повышает цену анализа | Root/modified RAVA/native injection остаются способами извлечения; не отвечает требованию абсолютной confidentiality |

Модели, текстуры, звук и всё остальное, что нужно отдать renderer или показать игроку, от копирования не защитить. Vault может держать на backend генератор или правила, но опубликованный результат получателю всё равно доступен.

Hardware attestation — это проверка evidence по заданной policy. Она не делает приложение безопасным автоматически и не закрывает все side channels. SEV-SNP защищает guest от некоторых угроз со стороны host/hypervisor. Если же недоверенный получатель управляет самим guest или запускает произвольный native code в защищённом worker, граница защиты нарушена.

## Что считается ценным ядром

Подходят proprietary generation rules, собственные процедурные алгоритмы автора, серверная проверка лицензий, сложные batch decisions, экономические правила и правила progression, если их можно применить к сериализованному input. Лучший контракт возвращает конечный результат или ограниченный effect plan.

Rendering, каждое чтение block state, каждый predicate в entity tick и операции, которым нужен свободный доступ к Minecraft heap, без изменения архитектуры удалённо вызывать нельзя. Автор должен явно выделить ценное ядро. Превратить произвольный готовый mod jar в объект, который нельзя сдампить, автоматически не получится.

## Компоненты

1. Publisher tool собирает public bridge и private core по отдельности, фиксирует API contract и version digest и подписывает manifest. Private core в bridge jar не входит.
2. Loader bridge SDK содержит адаптеры для Forge, Fabric и NeoForge. Он участвует в обычной регистрации модов, events и Mixins и превращает inputs в ограниченные immutable messages.
3. RAVA module manager проверяет public manifest и отвечает за sessions, typed serialization, quotas, async batches и применение results. Его интерфейс не зависит от программы: интеграция с Minecraft живёт в SDK или моде, а не в policy VM.
4. Entitlement service проверяет, есть ли у account или server instance право на module/version, и выдаёт короткую session capability. Решение принимает backend.
5. Execution worker владеет private implementation и private state. Он не загружает произвольные клиентские mods, plugins, JNI или patches.
6. Result applier на стороне Minecraft проверяет epoch, world version и entity handles и применяет effect plan в детерминированном порядке на owner thread.

На машине, которую контролирует получатель, RAVA module manager не может быть надёжным verifier. Подписи и проверки на клиенте помогают поддерживаемому клиенту, но решения об авторизации и выдаче секретов принимает backend или hardware-backed verifier.

## Контейнер и подпись

Предлагаемое расширение — .rvault. Это новый формат и API; в текущем репозитории их пока нет.

Public manifest содержит formatVersion, publisherId, moduleId, moduleVersion, executionMode, apiDigest, publicBridgeDigest, loaderCompatibility, exports, input/output size limits, capabilities, allowed protocol version и signingKeyId.

В remote mode публичный контейнер содержит manifest и bridge metadata, а private payload хранится на worker. Ciphertext и ключи на клиент тогда не попадают вовсе.

Publisher подписывает canonical manifest и digests payloads, а не произвольную JSON serialization. Подпись отвечает за origin и integrity, шифрование — за confidentiality. Использовать готовые проверенные реализации Ed25519 и AEAD, собственный шифр не писать. Key rotation и key revocation входят в publisher trust store.

Шифрование payloads для storage и attested worker должно привязывать publisher/module/version/apiDigest через authenticated additional data. Nonce каждого ciphertext должен быть уникален для своего ключа. Ключи private core не попадают в bridge, diagnostic output, environment dumps и клиентские capability tokens.

## Предлагаемый API

API работает с typed values и opaque handles и намеренно не использует Java object references:

    ProtectedModules.open(moduleId, apiDigest, entitlement)
    ModuleSession.submitBatch(worldEpoch, requestSequence, deadline, inputs)
    CompletionStage<ResultBatch>
    ResultBatch.effects
    ModuleSession.close()

Это набросок будущего API, а не существующие методы RAVA. SDK может генерировать typed interface из contract, чтобы автору мода не приходилось работать с сырыми payloads.

ResultBatch содержит module/version/apiDigest, session epoch, request sequence, input digest, упорядоченные result records и status. Перед применением клиент проверяет freshness и world identity. Immutable input snapshots создаются на owner thread, и указателя на Java heap worker не получает.

У ошибок явные outcomes: Denied, Expired, IncompatibleContract, StaleWorld, DeadlineExceeded, ServiceUnavailable, ResourceLimit, WorkerFailure. Ни одна из них не должна автоматически отключать проверку лицензии или приводить к выдаче private core.

## Session lifecycle

1. Public bridge регистрируется стандартным loader. Module manager проверяет signed manifest и соответствие API/bridge digests.
2. Backend аутентифицирует account или server instance и проверяет entitlement. Лицензия проверяется на сервере для каждого действия, значимого для авторизации.
3. Backend открывает capability, ограниченную module/version/exports, session epoch, expiry, quota и session/channel binding. Public build hash, присланный обычным клиентом, attestation не является.
4. Клиент передаёт bounded batch с sequence number, epoch и deadline через authenticated transport.
5. Worker проверяет contract, capability, limits, replay policy и inputs, выполняет private code и возвращает bounded result. Байты core в ответе не отправляются никогда.
6. Result applier отбрасывает устаревшие результаты и применяет валидные effects на owner thread в заданном порядке.
7. Renewal, expiry и revocation закрывают доступ на backend. У in-flight requests определённый исход. Если сервис недоступен, world save или checkpoint от этого не теряется.

Для повторов idempotent operation backend связывает result с request ID и input digest. Повтор с тем же ID, но другим input отвергается. В stateful actions обработка sequence и epoch не даёт применить действие дважды. Локальные часы не считаются источником истины для entitlement expiry.

Сессию, которой полностью управляет adversarial client, можно relay-ить или использовать как oracle. Quotas, ограниченные exports и состояние на backend сокращают злоупотребления, но не доказывают, что доступ нельзя передать другому человеку. Hardware session keys дают дополнительные свойства только при настоящей attestation защищённой среды.

## Совместимость с Forge, Fabric и NeoForge

Обычный bridge jar виден loader как есть: metadata, event registration, public resources, Mixins и debugging работают стандартно. Private core не объявляется обычным class resource и не передаётся transformers.

Mixin применяется к class bytecode до definition, а transformed classes можно экспортировать стандартными debugging tools. Значит, sensitive implementation, встроенная в такой target или Mixin, проходит через доступный loader pipeline. Поэтому private core выносится за его границу. Публичные wrappers можно скопировать, и core при этом не раскрывается.

В remote core и в правильно изолированном attested worker клиентские Mixins, ASM, reflection и access transformers не получают private core, который можно было бы изменить. Они работают с public bridge и классами Minecraft. Это явная граница совместимости: авторам, которым нужно смешивать чужие callbacks и objects с private internals, придётся переработать контракт. Для local licensed container под контролем получателя такой гарантии нет.

Native modules и Unsafe в процессе Minecraft не должны иметь доступа к адресному пространству private worker. В remote MVP это обеспечивается тем, что worker работает не на клиентской машине. Обычный локальный child process сам по себе нужной границы безопасности OS не создаёт: родительский процесс или native mod под тем же OS account может читать его память. Нужны отдельная identity, ограниченные process permissions и sandbox policy. От владельца OS с правами admin/root такая программная изоляция не защищает.

## Производительность

DRM не должен добавлять синхронный network round trip к каждой операции горячего tick loop. Для обычного server tick при 20 TPS бюджет составляет 50 ms, и network jitter может съесть его целиком, как бы хорошо ни работал JIT.

Использовать coarse-grained batches, асинхронную подготовку работы, ограниченные сообщения, pooling buffers и применение результатов на owner thread. Лучше всего подходят generation/precomputation и decisions с заранее известным сроком. Inputs должны передавать как можно меньше world snapshot.

Fallback для каждого export задаёт автор. Допустимы ожидание вне критического tick, отказ от нового protected action, ранее разрешённый result или публичная degraded functionality. Если ради автономного fallback передать private implementation, гарантия confidentiality меняется, и для этого нужен отдельный local mode.

TEE worker рядом с Minecraft host может снизить latency, но добавляет serialization, накладные расходы на границе enclave/VM и свои ограничения. Ни remote mode, ни TEE не ускоряют VM сами по себе. Производительность RAVA/JVM без DRM и overhead выбранного режима DRM сравнивать по отдельности.

## Сценарии угроз и ожидаемый результат

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

Kernel drivers, скрытое сканирование чужих процессов и destructive reactions не внедрять. Надёжной границы конфиденциальности они не дают, а стоимость поддержки и риск аварий повышают.

## План реализации

Этап 1, proof of boundary. Один public bridge и одна private batch operation на отдельном worker автора. Зафиксировать contract, entitlements, expiry, serialization и errors. Проверить дампы пакетов, классов и памяти клиента: private core не должен передаваться вовсе. Этап проверяет protocol design и не пытается доказать отсутствие любых side channels.

Этап 2, интеграция с RAVA. Generic module manager и SDK для одной поддерживаемой версии loader. Добавить quotas, ordered result applier, отказ от stale world, очистку и отмену сессий. Измерить overhead и latency под нагрузкой, проверить согласованность save при отказе сервиса.

Этап 3, publisher tooling и три loader. Signed manifests, reproducible bridge digests, key rotation, diagnostic privacy, адаптеры Forge/Fabric/NeoForge, compatibility matrix и migration policy. Версии loader нужно закреплять и тестировать по отдельности.

Этап 4, attested worker. Выбрать hardware platform под реальные требования развёртывания. Проверять fresh attestation, measurements образа worker, модуля и contract, security patch level и debug policy. Выдачу secret key привязывать к этому evidence и к защищённому session key. Произвольные mods в trusted worker не загружать.

Local licensed container — отдельный дополнительный режим продукта с более слабой гарантией. Выпускать его с обещанием, что дамп невозможен, нельзя.

## Критерии готовности

- Private core отсутствует в клиентском distribution, class resources, IR/JIT dumps, crash reports и network responses.
- Backend авторизует операции независимо от проверок на клиенте; expired, revoked и wrong-module capabilities отвергаются.
- Bounded schema отсекает oversized и malformed messages; output не может выполнить произвольный code на клиенте.
- У replay, retries, world restart, stale epochs и partial application детерминированная семантика.
- Service outage и worker crash не портят world save и не раскрывают core.
- Все public bridges работают с заявленными версиями loader и модов, а incompatible signatures дают понятную ошибку.
- p95/p99 latency и overhead измерены на целевых нагрузках; горячий tick не блокируется на непредсказуемом RPC.
- Для TEE отдельно проверены сценарии hostile host, debug и snapshot, а также trust assumptions. На обычном клиенте эти свойства автоматически заявлять нельзя.

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
