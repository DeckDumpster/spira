-- The beads schema and its seed rows, as a bd init leaves them. Do not edit.
-- Regenerate: testenv testdb dump-schema --bd bd --dolt dolt --root <scratch dir outside any git checkout> --out testenv/fixtures/beads-schema.sql
SET FOREIGN_KEY_CHECKS=0;
SET UNIQUE_CHECKS=0;
DROP TABLE IF EXISTS `bd_events_journal`;
CREATE TABLE `bd_events_journal` (
  `seq` bigint NOT NULL,
  `ts` datetime NOT NULL,
  `op` varchar(32) NOT NULL,
  `issue_id` varchar(255) NOT NULL,
  `issue_json` longtext,
  `dep_json` longtext,
  `comment_json` longtext,
  `actor` varchar(255) NOT NULL DEFAULT '',
  PRIMARY KEY (`seq`),
  KEY `idx_bd_events_journal_issue` (`issue_id`),
  KEY `idx_bd_events_journal_ts` (`ts`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `bd_events_seq`;
CREATE TABLE `bd_events_seq` (
  `id` int NOT NULL,
  `next_seq` bigint NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
INSERT INTO `bd_events_seq` (`id`,`next_seq`) VALUES (0,0);
DROP TABLE IF EXISTS `child_counters`;
CREATE TABLE `child_counters` (
  `parent_id` varchar(255) NOT NULL,
  `last_child` int NOT NULL DEFAULT '0',
  PRIMARY KEY (`parent_id`),
  CONSTRAINT `fk_counter_parent` FOREIGN KEY (`parent_id`) REFERENCES `issues` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `comments`;
CREATE TABLE `comments` (
  `id` char(36) NOT NULL,
  `issue_id` varchar(255) NOT NULL,
  `author` varchar(255) NOT NULL,
  `text` longtext NOT NULL,
  `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (`id`),
  KEY `idx_comments_created_at` (`created_at`),
  KEY `idx_comments_issue` (`issue_id`),
  KEY `idx_comments_issue_created_id` (`issue_id`,`created_at`,`id`),
  CONSTRAINT `fk_comments_issue` FOREIGN KEY (`issue_id`) REFERENCES `issues` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `compaction_snapshots`;
CREATE TABLE `compaction_snapshots` (
  `id` char(36) NOT NULL,
  `issue_id` varchar(255) NOT NULL,
  `compaction_level` int NOT NULL,
  `snapshot_json` blob NOT NULL,
  `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (`id`),
  KEY `idx_comp_snap_issue` (`issue_id`,`compaction_level`,`created_at` DESC),
  CONSTRAINT `fk_comp_snap_issue` FOREIGN KEY (`issue_id`) REFERENCES `issues` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `config`;
CREATE TABLE `config` (
  `key` varchar(255) NOT NULL,
  `value` text NOT NULL,
  PRIMARY KEY (`key`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
INSERT INTO `config` (`key`,`value`) VALUES ('auto_compact_enabled','false'), ('compact_batch_size','50'), ('compact_parallel_workers','5'), ('compact_tier1_days','30'), ('compact_tier1_dep_levels','2'), ('compact_tier2_commits','100'), ('compact_tier2_days','90'), ('compact_tier2_dep_levels','5'), ('compaction_enabled','false'), ('issue_prefix','sp');
DROP TABLE IF EXISTS `custom_statuses`;
CREATE TABLE `custom_statuses` (
  `name` varchar(64) NOT NULL,
  `category` varchar(32) NOT NULL DEFAULT 'unspecified',
  PRIMARY KEY (`name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `custom_types`;
CREATE TABLE `custom_types` (
  `name` varchar(64) NOT NULL,
  PRIMARY KEY (`name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `dependencies`;
CREATE TABLE `dependencies` (
  `id` char(36) NOT NULL,
  `issue_id` varchar(255) NOT NULL,
  `type` varchar(32) NOT NULL DEFAULT 'blocks',
  `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  `created_by` varchar(255) NOT NULL,
  `metadata` json DEFAULT (json_object()),
  `thread_id` varchar(255) DEFAULT '',
  `depends_on_issue_id` varchar(255),
  `depends_on_wisp_id` varchar(255),
  `depends_on_external` varchar(255),
  PRIMARY KEY (`id`),
  KEY `idx_dep_external_target` (`depends_on_external`),
  KEY `idx_dep_issue_target` (`depends_on_issue_id`),
  KEY `idx_dep_type_external` (`type`,`depends_on_external`),
  KEY `idx_dep_type_issue` (`type`,`depends_on_issue_id`),
  KEY `idx_dep_type_wisp` (`type`,`depends_on_wisp_id`),
  KEY `idx_dep_wisp_target` (`depends_on_wisp_id`),
  KEY `idx_dependencies_issue` (`issue_id`),
  KEY `idx_dependencies_thread` (`thread_id`),
  UNIQUE KEY `uk_dep_external_target` (`issue_id`,`depends_on_external`),
  UNIQUE KEY `uk_dep_issue_target` (`issue_id`,`depends_on_issue_id`),
  UNIQUE KEY `uk_dep_wisp_target` (`issue_id`,`depends_on_wisp_id`),
  CONSTRAINT `fk_dep_issue` FOREIGN KEY (`issue_id`) REFERENCES `issues` (`id`) ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT `fk_dep_issue_target` FOREIGN KEY (`depends_on_issue_id`) REFERENCES `issues` (`id`) ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT `ck_dep_one_target` CHECK (((((NOT(`depends_on_issue_id` IS NULL)) + (NOT(`depends_on_wisp_id` IS NULL))) + (NOT(`depends_on_external` IS NULL))) = 1))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `events`;
CREATE TABLE `events` (
  `issue_id` varchar(255) NOT NULL,
  `event_type` varchar(32) NOT NULL,
  `actor` varchar(255) NOT NULL,
  `old_value` longtext,
  `new_value` longtext,
  `comment` text,
  `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  `id` char(36) NOT NULL,
  PRIMARY KEY (`id`),
  KEY `idx_events_created_at` (`created_at`),
  KEY `idx_events_issue` (`issue_id`),
  CONSTRAINT `fk_events_issue` FOREIGN KEY (`issue_id`) REFERENCES `issues` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `federation_peers`;
CREATE TABLE `federation_peers` (
  `name` varchar(255) NOT NULL,
  `remote_url` varchar(1024) NOT NULL,
  `username` varchar(255),
  `password_encrypted` blob,
  `sovereignty` varchar(8) DEFAULT '',
  `last_sync` datetime,
  `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  `updated_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  PRIMARY KEY (`name`),
  KEY `idx_federation_peers_sovereignty` (`sovereignty`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `ignored_schema_migrations`;
CREATE TABLE `ignored_schema_migrations` (
  `version` int NOT NULL,
  `applied_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  `content_hash` char(64),
  PRIMARY KEY (`version`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
INSERT INTO `ignored_schema_migrations` (`version`,`applied_at`,`content_hash`) VALUES (1,'2026-11-24 23:07:28','a73e9a486124f89e90d50f4597fd1d7ad08e5d0d654459ec87b7520b47f1bb5f'), (2,'2026-11-24 23:07:29','c575c6710169f2e697f3aca60f5f698acc8a64af0edb1e2305b9d377fd845f45'), (3,'2026-11-24 23:07:29','f82947deeafe589598e2a1802eddf17712bd322692f924bb5653b0fbd5c46a0d'), (4,'2026-11-24 23:07:30','12e18a8bdd214d6f96625e78998433ccc9c2d0ef4d1a35307f2653e725c800b9'), (5,'2026-11-24 23:07:30','08780b28ddef6c15f5de558ea08d821b52610f810555739ee1d737779986c607'), (6,'2026-11-24 23:07:30','4746f10d7040d0b7a9e127437356de87765d12d2d27ee37c9800191e6b619d14'), (7,'2026-11-24 23:07:31','c3c8d92846a54183cc99afc09f779f6f5ba8030c16ce46b3ee750314bf134627'), (8,'2026-11-24 23:07:31','51375854ec081d2e0b1434b5ff8b6d87092b891636214ebb9f1dbef22438cc1a'), (9,'2026-11-24 23:07:31','3d1ee4f9dde3342d110474c8090a30c474edafe2290e4372a3fc4607c1fdc030'), (10,'2026-11-24 23:07:32','59d6644667b7e34f9d23261cb694d461ddd4953969e75519d370b0cce8afd0e8'), (11,'2026-11-24 23:07:32','0efade0ecb620420ee42127d3f67230da108e31f2ff3c4281855a8dc9e187ecd'), (12,'2026-11-24 23:07:32','e128a1301a2bcb967f684b09318aa5cf7ed9adb5f6dd7d412de3d67d1de2b28d'), (13,'2026-11-24 23:07:33','127a4af7012769e5f016b13b198b7a57ff8386a10f8a68ffb9ff204989564d1b'), (14,'2026-11-24 23:07:33','29ad3654218700481b9541331c714ecbbe96dc81c04dc83fba6bf8d35bed7fa4'), (15,'2026-11-24 23:07:33','e761d5c3ceb9610edbb2e1d153e82d235cc7d7f0b87f28a45bec43835e6a4e19'), (16,'2026-11-24 23:07:33','31a55421b78d8c90db6cfba5bf8e99e87d6123271575282f761aa077e5d36386'), (17,'2026-11-24 23:07:34','1f031aaf2037280e62cbf604748a4157bf25e3a60b3392af7d74835c5059bdd8'), (18,'2026-11-24 23:07:34','3c2120bf55d15e71a9f96972007c3d914154d17ea37a2b51b44412e3e3df6ef1'), (19,'2026-11-24 23:07:34','db072d67a403e60d5bcbe924b84e473adb09f2326d36ede82fcdab17bb1e2a87'), (20,'2026-11-24 23:07:34','9ff476e6c73a3a477aec95143d5cbf180517a9484f647447392576f13181a015'), (21,'2026-11-24 23:07:34','3cfa1e07a3d02eb7f791814d9642a93b9af3bf63728943afa05dcb0e489919c8'), (22,'2026-11-24 23:07:35','3d0adcb9f5833b4ba70fb48f88e1043250796feb7173abe000ea4de85eb8b3a5'), (23,'2026-11-24 23:07:35','d0edd83ca4cfbc46bd17799c6497a70c8c011d130d08a00c71daf792ee1dcd51'), (24,'2026-11-24 23:07:36','69a96330e00643dfeddc4bf6ae9bf3b743a3cfddd86f16dc7b8912758b1b570d'), (25,'2026-11-24 23:07:36','c81ca9edc80c25f6ab4f92d05d6ea412a780b108d1ed83745027440e9723dceb'), (26,'2026-11-24 23:07:36','dc5c1b9a74e42184451796068ecba1ca11d8ebd0fe7e8c4bd4900d0cfee30446');
DROP TABLE IF EXISTS `interactions`;
CREATE TABLE `interactions` (
  `id` varchar(32) NOT NULL,
  `kind` varchar(64) NOT NULL,
  `created_at` datetime NOT NULL,
  `actor` varchar(255),
  `issue_id` varchar(255),
  `model` varchar(255),
  `prompt` text,
  `response` text,
  `error` text,
  `tool_name` varchar(255),
  `exit_code` int,
  `parent_id` varchar(32),
  `label` varchar(64),
  `reason` text,
  `extra` json,
  PRIMARY KEY (`id`),
  KEY `idx_interactions_created_at` (`created_at`),
  KEY `idx_interactions_issue_id` (`issue_id`),
  KEY `idx_interactions_kind` (`kind`),
  KEY `idx_interactions_parent_id` (`parent_id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `issue_counter`;
CREATE TABLE `issue_counter` (
  `prefix` varchar(255) NOT NULL,
  `last_id` int NOT NULL DEFAULT '0',
  PRIMARY KEY (`prefix`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `issue_snapshots`;
CREATE TABLE `issue_snapshots` (
  `id` char(36) NOT NULL,
  `issue_id` varchar(255) NOT NULL,
  `snapshot_time` datetime NOT NULL,
  `compaction_level` int NOT NULL,
  `original_size` int NOT NULL,
  `compressed_size` int NOT NULL,
  `original_content` text NOT NULL,
  `archived_events` text,
  PRIMARY KEY (`id`),
  KEY `idx_snapshots_issue` (`issue_id`),
  KEY `idx_snapshots_level` (`compaction_level`),
  CONSTRAINT `fk_snapshots_issue` FOREIGN KEY (`issue_id`) REFERENCES `issues` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `issues`;
CREATE TABLE `issues` (
  `id` varchar(255) NOT NULL,
  `content_hash` varchar(64),
  `title` varchar(500) NOT NULL,
  `description` longtext NOT NULL,
  `design` longtext NOT NULL,
  `acceptance_criteria` longtext NOT NULL,
  `notes` longtext NOT NULL,
  `status` varchar(32) NOT NULL DEFAULT 'open',
  `priority` int NOT NULL DEFAULT '2',
  `issue_type` varchar(32) NOT NULL DEFAULT 'task',
  `assignee` varchar(255),
  `estimated_minutes` int,
  `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  `created_by` varchar(255) DEFAULT '',
  `owner` varchar(255) DEFAULT '',
  `updated_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  `closed_at` datetime,
  `closed_by_session` varchar(255) DEFAULT '',
  `external_ref` varchar(255),
  `spec_id` varchar(1024),
  `compaction_level` int DEFAULT '0',
  `compacted_at` datetime,
  `compacted_at_commit` varchar(64),
  `original_size` int,
  `sender` varchar(255) DEFAULT '',
  `ephemeral` tinyint(1) DEFAULT '0',
  `wisp_type` varchar(32) DEFAULT '',
  `pinned` tinyint(1) DEFAULT '0',
  `is_template` tinyint(1) DEFAULT '0',
  `mol_type` varchar(32) DEFAULT '',
  `work_type` varchar(32) DEFAULT 'mutex',
  `source_system` varchar(255) DEFAULT '',
  `metadata` json DEFAULT (json_object()),
  `source_repo` varchar(512) DEFAULT '',
  `close_reason` longtext DEFAULT '',
  `event_kind` varchar(32) DEFAULT '',
  `actor` varchar(255) DEFAULT '',
  `target` varchar(255) DEFAULT '',
  `payload` text DEFAULT '',
  `await_type` varchar(32) DEFAULT '',
  `await_id` varchar(255) DEFAULT '',
  `timeout_ns` bigint DEFAULT '0',
  `waiters` text DEFAULT '',
  `hook_bead` varchar(255) DEFAULT '',
  `role_bead` varchar(255) DEFAULT '',
  `agent_state` varchar(32) DEFAULT '',
  `last_activity` datetime,
  `role_type` varchar(32) DEFAULT '',
  `rig` varchar(255) DEFAULT '',
  `due_at` datetime,
  `defer_until` datetime,
  `no_history` tinyint(1) DEFAULT '0',
  `started_at` datetime,
  `is_blocked` tinyint(1) NOT NULL DEFAULT '0',
  `row_lock` bigint NOT NULL DEFAULT '0',
  `storage_class` varchar(16),
  PRIMARY KEY (`id`),
  KEY `idx_issues_assignee` (`assignee`),
  KEY `idx_issues_created_at` (`created_at`),
  KEY `idx_issues_defer_until` (`defer_until`),
  KEY `idx_issues_external_ref` (`external_ref`),
  KEY `idx_issues_is_blocked` (`is_blocked`,`status`),
  KEY `idx_issues_issue_type` (`issue_type`),
  KEY `idx_issues_priority` (`priority`),
  KEY `idx_issues_spec_id` (`spec_id`),
  KEY `idx_issues_status_updated_at` (`status`,`updated_at`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `labels`;
CREATE TABLE `labels` (
  `issue_id` varchar(255) NOT NULL,
  `label` varchar(255) NOT NULL,
  PRIMARY KEY (`issue_id`,`label`),
  KEY `idx_labels_label` (`label`),
  CONSTRAINT `fk_labels_issue` FOREIGN KEY (`issue_id`) REFERENCES `issues` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `leases`;
CREATE TABLE `leases` (
  `issue_id` varchar(255) NOT NULL,
  `holder` varchar(255) NOT NULL,
  `granted_at` datetime NOT NULL,
  `lease_expires_at` datetime NOT NULL,
  `heartbeat_at` datetime NOT NULL,
  `granted_node` varchar(255) NOT NULL DEFAULT '',
  PRIMARY KEY (`issue_id`),
  KEY `idx_leases_expires` (`lease_expires_at`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `local_metadata`;
CREATE TABLE `local_metadata` (
  `key` varchar(255) NOT NULL,
  `value` text NOT NULL DEFAULT '',
  PRIMARY KEY (`key`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
INSERT INTO `local_metadata` (`key`,`value`) VALUES ('bd_version','1.3.1');
DROP TABLE IF EXISTS `metadata`;
CREATE TABLE `metadata` (
  `key` varchar(255) NOT NULL,
  `value` text NOT NULL,
  PRIMARY KEY (`key`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
INSERT INTO `metadata` (`key`,`value`) VALUES ('_project_id','977b8d23-0d9e-41cc-8a0d-ce91e2fce8e7'), ('clone_id','1107eda99e6794eb'), ('last_import_time','2026-11-24T23:07:41-08:00'), ('repo_id','ca1c565f07029ab57795431022d26b03');
DROP TABLE IF EXISTS `provenance_events`;
CREATE TABLE `provenance_events` (
  `id` char(36) NOT NULL,
  `issue_id` varchar(255) NOT NULL,
  `kind` varchar(32) NOT NULL,
  `actor` varchar(255),
  `ref` varchar(255),
  `ref_kind` varchar(32),
  `payload` text,
  `source` varchar(64) NOT NULL,
  `occurred_at` datetime,
  `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (`id`),
  KEY `idx_provenance_events_issue_occurred` (`issue_id`,`occurred_at`),
  KEY `idx_provenance_events_kind` (`kind`),
  KEY `idx_provenance_events_ref` (`ref`),
  CONSTRAINT `fk_provenance_events_issue` FOREIGN KEY (`issue_id`) REFERENCES `issues` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `repo_mtimes`;
CREATE TABLE `repo_mtimes` (
  `repo_path` varchar(512) NOT NULL,
  `jsonl_path` varchar(512) NOT NULL,
  `mtime_ns` bigint NOT NULL,
  `last_checked` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (`repo_path`),
  KEY `idx_repo_mtimes_checked` (`last_checked`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `routes`;
CREATE TABLE `routes` (
  `prefix` varchar(32) NOT NULL,
  `path` varchar(512) NOT NULL,
  `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  `updated_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  PRIMARY KEY (`prefix`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `schema_migrations`;
CREATE TABLE `schema_migrations` (
  `version` int NOT NULL,
  `content_hash` char(64),
  PRIMARY KEY (`version`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
INSERT INTO `schema_migrations` (`version`,`content_hash`) VALUES (1,'0c9bc4d97e2c5ee5d276c952333b934cd7d7d18210997d6bfd72d17f0fdb14f0'), (2,'9ca011fa0b24ddf26f689eebf347cc2f80dc90256b099d91b993ec82c4d74da5'), (3,'1780f0dc941d97b74af53d5603646d05e83d94eee5a02ac68ccf929200deaec0'), (4,'35ac76b3d0b2d6287cad1329c77c8a0562bb6310ba0ac34ae273d72d4bed0e2a'), (5,'c8cbe4bebbe0f92c8a0ccc2a1a0d11ad442635811c2dab093c34c9341a8499b2'), (6,'435f5ef129dd4999a9b393c98d633038f0e443280a26e63f7d4315bf025dab1f'), (7,'f92c4d3dd32cae489142de34c4706900736c91cdc53b0d54e87e71656b828dc9'), (8,'99db9b4b5a785ddd209dff628364c4c1eff549c3ca5ec48a64c604b15ef95bfb'), (9,'269ef83f083766b7f2fd2dfd2814ef36a585ae4aa18bcab0468a713d48595e48'), (10,'cb34cbe311fd89a17d72067f449a65089f2fc9a7c4b3774d54448db5ebc3d659'), (11,'bde5204466541dddb53a652fe58561c08b6b3c1fa7feb38a0a582d432d76bda6'), (12,'68e8ef1fc336e8ca6973f6b927b09d3206a7ebd96db9c4cdceab52fb8498c431'), (13,'7ddb30dabe32a66197de78f62400f41d000cf43d0ba8ba2a3943536504dddcdb'), (14,'0e17f1cb9fc5ee5a7c43ca8e02b9fd59814f8fd4372f9ffb17b50c34388614ce'), (15,'6aa04b3f66566b243c9a4fc09136af5c17f0ffe947a489245e3c451b995a59b9'), (16,'5d556c01404eb06acfb9df2b20fde36a2e91bd8a7d14462acbd3c0ae928ea286'), (17,'4bab7acbe2d8e441f156cc5912f421dcb214b9b08d7958eacc8e3a7318ab5f57'), (18,'84dc6a3950a5447e6665e3d98b43f11a7631dc4e53bab1be332d135e51c9511f'), (19,'3df660feb6b64c84ef74c408fe8f5840a2815936534362ed123122f795a21c26'), (20,'bca95662441635454b879bb44ca1e607df9c5efd8fd802be3d53a7371a5ced86'), (21,'d19284a2be70a448db7746ae009cb264dc86dac0d9094aae9fe80a713a08ee70'), (22,'fc1375e64620cd4264516718d8f924d88131881f4c00c51471b7630c11b74a45'), (23,'d11cd7cb462239e49c0b2f9b93ac73771106d5450ebc3422bb7737748c6f967b'), (24,'816da3feeed531ca9787dca8d4b8072dd94b64d573cf4b6751c61f2390c88a68'), (25,'09782d195f22d620ba41802856f9eb6e1a10f68bc5bcc8f3616624f8c28b576a'), (26,'33daef6b5a07364fc5f59ab59b0f5c677dcf98d9bd0866c7c3bea874f1f01348'), (27,'dd3b58a8bbed4c49e855a6002981a285f10b6094938534dd3da9b121155f24d6'), (28,'24a816406a6f40c66db98697869d172b2b4366b107788fdd00f44b99abee45c3'), (29,'b989fc49ab03e06ff6520f150d6958efdbd3f444b69b824f7b4bb283ca6f2b65'), (30,'8d55135274c1c75e722e802b250911cc574d52a4ba74fbb13edde84f30f9b3a4'), (31,'2d5cf3a6d4245b48c1b893efcc17b9fa25f1ea807e61290cf54064451fc58d93'), (32,'42b86d6b7352f7e158532cc67f4afa142dc2503ce9f780733c06fc9da3478751'), (33,'9256746bb0d6ad9ed9afba0f0501259f0a3599b7d7da507dd7d808a13c688c75'), (34,'7add95ead971c57618cfbc52e5cb6771d872785bfc035bde12cd27a76358848e'), (35,'f917fe319540155572f5c51618cbbe20ec8b9b669d5926d68e3a62629c317c5a'), (36,'e55f1870c2bc031c574b495ebca650365a9c8088d7a8968fb86a19cab81267e8'), (37,'7526beadee13aa7d6ee2175e9a543210b07c1a4426c382f251ebd31eebe32745'), (38,'002dc637d45840487c35a01ce15cee04872baa45c21f8e20170a4339135a38e7'), (39,'db65f241d1e82da41f34b960abc912f78b72e7dc1cb6dd2298974308edf8b768'), (40,'c7068d67ad3e2b4852549e04d97f25814121de83db919e220bdcb9bdd9b61e5e'), (41,'e84ef393576df31550570fd3217579b1880850c0645b7d2b6ad8cf55a18ac1c0'), (42,'9d0fbb3d02955d25442c4a628ac518236068a97ecf164edbc814216bd5a55adc'), (43,'548b8a69ab82179b7ee7ab3cc87ef00b3ae28d5a3a4976f42364f6939e384668'), (44,'531524854cc0e2361817d1ad94ba9470b0e81e378a5422ffb8bce71f3500adf9'), (45,'395dcbf186deedb592d8ed144e0b7953455efa0ccf02076d3b63e5e5d05dc241'), (46,'4d6a5a5fc5e386f2c538b4f3574b82fb2ba025176997953a0a4fbd2a91cddf68'), (47,'4aff5a7cc2e1e0250ce4cdb911d010c936681871fb627bceee4b2dc4647dd0a1'), (48,'093c7a41145508879844d16cbe5acfa69baddbb19bf3e05f00bc08720adea657'), (49,'3359343efb7c28e5803de07e9088670489615d151843d7f23705f3ca8dd6a1e8'), (50,'96265ff898753f3bb34549fafa18bbaeb67519705b8cf4e32e67f1058df6841a'), (51,'deb741d5ac5fc1e0cd2e60eb47e84f384ed3455b18d3e2f12cb4e935fe461059'), (52,'60ae32bc26c41da2e743c1172f73930d18482b8a98aea08b9de85f6009f9c01d'), (53,'f13909f0db89f48db694a782d1805eee7ee1b2b2e4dcb3d4b12352333cbcb330'), (54,'2e51058ba15be9d87415949bb03c2c7c2f0e0743c5ae0d3e819b617d95f61680'), (55,'2237ddbe13f5a783c65da3bb1292d9a5d89a5f22fddb0c33d2db57f1492df22c'), (56,'acc28e9b1aa7272c16c86fcb9fd4a2038ae47729a60b53bb75f3a9df05cd3a82'), (57,'f86e0f77be2e58affe1c485767b8dd450408d782c0413a41948ca0c0b6d72d06'), (58,'9c1c6d557a03e71cf6b49123831c9f5b5f910fcea3ca51e982ec4acce04ea6ec'), (59,'f0186029a1c87d579b67e0540ec22ad0bd0fc59793b07f7d24a24e122b77c394'), (60,'40419632ba75d92dfcf08e16e77a780f014172a17068f6021937df9086a24588'), (61,'d2d65b92789f137da8ee586067c862a9c7744921ccac421e1b793f42050b62a2'), (62,'c7de6089997344784a71e7d82ea5e417baaf66320b4533d225a4a6bc3c74222a'), (63,'1ef173be07ca58d17cb09ba3b3f5ca0c96edfcace8d52ae3adde2fd27e8248bd'), (64,'25ea9ad8e849dc7e32e7c5850631bedc63a5c5da3bdc58ccb2762fd7a6c758fc'), (65,'c7341bb977daff92e073db2579e63058cef88d43f1e09b13fa153a772caac2a6'), (66,'a524d42ba0eced9350edbf6402dc2952957ff8f75a896bc8339c4cbc8596c793');
DROP TABLE IF EXISTS `wisp_child_counters`;
CREATE TABLE `wisp_child_counters` (
  `parent_id` varchar(255) NOT NULL,
  `last_child` int NOT NULL DEFAULT '0',
  PRIMARY KEY (`parent_id`),
  CONSTRAINT `fk_wisp_child_counters_parent` FOREIGN KEY (`parent_id`) REFERENCES `wisps` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `wisp_comments`;
CREATE TABLE `wisp_comments` (
  `id` char(36) NOT NULL,
  `issue_id` varchar(255) NOT NULL,
  `author` varchar(255) DEFAULT '',
  `text` longtext NOT NULL,
  `created_at` datetime DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (`id`),
  KEY `idx_wisp_comments_issue` (`issue_id`),
  KEY `idx_wisp_comments_issue_created_id` (`issue_id`,`created_at`,`id`),
  CONSTRAINT `fk_wisp_comments_issue` FOREIGN KEY (`issue_id`) REFERENCES `wisps` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `wisp_dependencies`;
CREATE TABLE `wisp_dependencies` (
  `id` char(36) NOT NULL,
  `issue_id` varchar(255) NOT NULL,
  `depends_on_issue_id` varchar(255),
  `depends_on_wisp_id` varchar(255),
  `depends_on_external` varchar(255),
  `type` varchar(32) NOT NULL DEFAULT 'blocks',
  `created_at` datetime DEFAULT CURRENT_TIMESTAMP,
  `created_by` varchar(255) DEFAULT '',
  `metadata` json DEFAULT (json_object()),
  `thread_id` varchar(255) DEFAULT '',
  PRIMARY KEY (`id`),
  KEY `fk_wisp_dep_issue_target` (`depends_on_issue_id`),
  KEY `fk_wisp_dep_wisp_target` (`depends_on_wisp_id`),
  KEY `idx_wisp_dep_type` (`type`),
  KEY `idx_wisp_dep_type_external` (`type`,`depends_on_external`),
  KEY `idx_wisp_dep_type_issue` (`type`,`depends_on_issue_id`),
  KEY `idx_wisp_dep_type_wisp` (`type`,`depends_on_wisp_id`),
  UNIQUE KEY `uk_wisp_dep_external_target` (`issue_id`,`depends_on_external`),
  UNIQUE KEY `uk_wisp_dep_issue_target` (`issue_id`,`depends_on_issue_id`),
  UNIQUE KEY `uk_wisp_dep_wisp_target` (`issue_id`,`depends_on_wisp_id`),
  CONSTRAINT `fk_wisp_dep_issue` FOREIGN KEY (`issue_id`) REFERENCES `wisps` (`id`) ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT `fk_wisp_dep_issue_target` FOREIGN KEY (`depends_on_issue_id`) REFERENCES `issues` (`id`) ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT `fk_wisp_dep_wisp_target` FOREIGN KEY (`depends_on_wisp_id`) REFERENCES `wisps` (`id`) ON DELETE CASCADE ON UPDATE CASCADE,
  CONSTRAINT `ck_wisp_dep_one_target` CHECK (((((NOT(`depends_on_issue_id` IS NULL)) + (NOT(`depends_on_wisp_id` IS NULL))) + (NOT(`depends_on_external` IS NULL))) = 1))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `wisp_events`;
CREATE TABLE `wisp_events` (
  `id` char(36) NOT NULL,
  `issue_id` varchar(255) NOT NULL,
  `event_type` varchar(32) NOT NULL,
  `actor` varchar(255) DEFAULT '',
  `old_value` longtext DEFAULT '',
  `new_value` longtext DEFAULT '',
  `comment` text DEFAULT '',
  `created_at` datetime DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (`id`),
  KEY `idx_wisp_events_created_at` (`created_at`),
  KEY `idx_wisp_events_issue` (`issue_id`),
  CONSTRAINT `fk_wisp_events_issue` FOREIGN KEY (`issue_id`) REFERENCES `wisps` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `wisp_labels`;
CREATE TABLE `wisp_labels` (
  `issue_id` varchar(255) NOT NULL,
  `label` varchar(255) NOT NULL,
  PRIMARY KEY (`issue_id`,`label`),
  KEY `idx_wisp_labels_label` (`label`),
  CONSTRAINT `fk_wisp_labels_issue` FOREIGN KEY (`issue_id`) REFERENCES `wisps` (`id`) ON DELETE CASCADE ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
DROP TABLE IF EXISTS `wisps`;
CREATE TABLE `wisps` (
  `id` varchar(255) NOT NULL,
  `content_hash` varchar(64),
  `title` varchar(500) NOT NULL,
  `description` longtext NOT NULL DEFAULT '',
  `design` longtext NOT NULL DEFAULT '',
  `acceptance_criteria` longtext NOT NULL DEFAULT '',
  `notes` longtext NOT NULL DEFAULT '',
  `status` varchar(32) NOT NULL DEFAULT 'open',
  `priority` int NOT NULL DEFAULT '2',
  `issue_type` varchar(32) NOT NULL DEFAULT 'task',
  `assignee` varchar(255),
  `estimated_minutes` int,
  `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
  `created_by` varchar(255) DEFAULT '',
  `owner` varchar(255) DEFAULT '',
  `updated_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  `closed_at` datetime,
  `closed_by_session` varchar(255) DEFAULT '',
  `external_ref` varchar(255),
  `spec_id` varchar(1024),
  `compaction_level` int DEFAULT '0',
  `compacted_at` datetime,
  `compacted_at_commit` varchar(64),
  `original_size` int,
  `sender` varchar(255) DEFAULT '',
  `ephemeral` tinyint(1) DEFAULT '0',
  `wisp_type` varchar(32) DEFAULT '',
  `pinned` tinyint(1) DEFAULT '0',
  `is_template` tinyint(1) DEFAULT '0',
  `mol_type` varchar(32) DEFAULT '',
  `work_type` varchar(32) DEFAULT 'mutex',
  `source_system` varchar(255) DEFAULT '',
  `metadata` json DEFAULT (json_object()),
  `source_repo` varchar(512) DEFAULT '',
  `close_reason` longtext DEFAULT '',
  `event_kind` varchar(32) DEFAULT '',
  `actor` varchar(255) DEFAULT '',
  `target` varchar(255) DEFAULT '',
  `payload` text DEFAULT '',
  `await_type` varchar(32) DEFAULT '',
  `await_id` varchar(255) DEFAULT '',
  `timeout_ns` bigint DEFAULT '0',
  `waiters` text DEFAULT '',
  `hook_bead` varchar(255) DEFAULT '',
  `role_bead` varchar(255) DEFAULT '',
  `agent_state` varchar(32) DEFAULT '',
  `last_activity` datetime,
  `role_type` varchar(32) DEFAULT '',
  `rig` varchar(255) DEFAULT '',
  `due_at` datetime,
  `defer_until` datetime,
  `no_history` tinyint(1) DEFAULT '0',
  `started_at` datetime,
  `row_lock` bigint NOT NULL DEFAULT '0',
  `storage_class` varchar(16),
  `is_blocked` tinyint(1) NOT NULL DEFAULT '0',
  PRIMARY KEY (`id`),
  KEY `idx_wisps_assignee` (`assignee`),
  KEY `idx_wisps_created_at` (`created_at`),
  KEY `idx_wisps_defer_until` (`defer_until`),
  KEY `idx_wisps_external_ref` (`external_ref`),
  KEY `idx_wisps_is_blocked` (`is_blocked`,`status`),
  KEY `idx_wisps_issue_type` (`issue_type`),
  KEY `idx_wisps_priority` (`priority`),
  KEY `idx_wisps_spec_id` (`spec_id`),
  KEY `idx_wisps_status` (`status`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_bin;
CREATE OR REPLACE VIEW blocked_issues AS
WITH done_frozen AS (
    SELECT name FROM custom_statuses WHERE category IN ('done', 'frozen')
)
SELECT
    i.*,
    (SELECT COUNT(*)
     FROM dependencies d
     WHERE d.issue_id = i.id
       AND d.type = 'blocks'
       AND EXISTS (
         SELECT 1 FROM issues blocker
         WHERE blocker.id = d.depends_on_issue_id
           AND blocker.status NOT IN ('closed', 'pinned')
           AND blocker.status NOT IN (SELECT name FROM done_frozen)
       )
    ) as blocked_by_count
FROM issues i
WHERE i.status NOT IN ('closed', 'pinned')
  AND i.status NOT IN (SELECT name FROM done_frozen)
  AND EXISTS (
    SELECT 1 FROM dependencies d
    WHERE d.issue_id = i.id
      AND d.type = 'blocks'
      AND EXISTS (
        SELECT 1 FROM issues blocker
        WHERE blocker.id = d.depends_on_issue_id
          AND blocker.status NOT IN ('closed', 'pinned')
          AND blocker.status NOT IN (SELECT name FROM done_frozen)
      )
  );
CREATE OR REPLACE VIEW ready_issues AS
WITH RECURSIVE
  blocked_directly AS (
    SELECT DISTINCT d.issue_id
    FROM dependencies d
    WHERE d.type = 'blocks'
      AND EXISTS (
        SELECT 1 FROM issues blocker
        WHERE blocker.id = d.depends_on_issue_id
          AND blocker.status NOT IN ('closed', 'pinned')
      )
  ),
  blocked_transitively AS (
    SELECT issue_id, 0 as depth
    FROM blocked_directly
    UNION ALL
    SELECT d.issue_id, bt.depth + 1
    FROM blocked_transitively bt
    JOIN dependencies d ON d.depends_on_issue_id = bt.issue_id
    WHERE d.type = 'parent-child'
      AND bt.depth < 50
  )
SELECT i.*
FROM issues i
LEFT JOIN blocked_transitively bt ON bt.issue_id = i.id
WHERE (
    i.status = 'open'
    OR i.status IN (SELECT name FROM custom_statuses WHERE category = 'active')
  )
  AND (i.ephemeral = 0 OR i.ephemeral IS NULL)
  AND bt.issue_id IS NULL
  AND (i.defer_until IS NULL OR i.defer_until <= UTC_TIMESTAMP())
  AND NOT EXISTS (
    SELECT 1 FROM dependencies d_parent
    JOIN issues parent ON parent.id = d_parent.depends_on_issue_id
    WHERE d_parent.issue_id = i.id
      AND d_parent.type = 'parent-child'
      AND parent.defer_until IS NOT NULL
      AND parent.defer_until > UTC_TIMESTAMP()
  );
