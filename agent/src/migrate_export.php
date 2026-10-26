<?php
// Run by `vercelify-agent migrate` inside Coolify's container
// (`docker exec -i coolify php`): prints everything the migration takes over
// from Coolify as one JSON document, secrets decrypted with Coolify's own
// APP_KEY. Read-only: plain queries, no models (some of Coolify's accessors
// write back). Verified against Coolify 4.4.6 (COOLIFY_TESTED_VERSION).
// Every column is read with `col()`, so one a version lacks is null.

require '/var/www/html/vendor/autoload.php';
$app = require '/var/www/html/bootstrap/app.php';
$app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();

use Illuminate\Support\Facades\Crypt;
use Illuminate\Support\Facades\DB;

function dec($v) {
    if ($v === null || $v === '') return $v;
    // `encrypt()` (serialized: the `encrypted` cast in this Laravel), else
    // `encryptString()`.
    try { return decrypt($v); } catch (\Throwable $e) {}
    try { return Crypt::decryptString($v); } catch (\Throwable $e) {}
    return $v; // stored in clear (older rows)
}
function col($row, $k) { return property_exists($row, $k) ? $row->$k : null; }
function rows($table, $where = []) {
    if (!DB::getSchemaBuilder()->hasTable($table)) return [];
    $q = DB::table($table);
    if (DB::getSchemaBuilder()->hasColumn($table, 'deleted_at')) $q->whereNull('deleted_at');
    foreach ($where as $k => $v) $q->where($k, $v);
    return $q->orderBy('id')->get()->all();
}

$servers = [];
foreach (rows('servers') as $s) $servers[$s->id] = ['name' => $s->name, 'ip' => $s->ip, 'local' => $s->id == 0];
$dest = [];
foreach (rows('standalone_dockers') as $d) $dest[$d->id] = $d->server_id;
function server_of($r, $dest, $servers) {
    $sid = $dest[col($r, 'destination_id')] ?? 0;
    return $servers[$sid] ?? ['name' => "server $sid", 'ip' => '', 'local' => false];
}

$envs = [];
foreach (rows('environments') as $e) $envs[$e->id] = $e;
$projects = [];
foreach (rows('projects') as $p) {
    $projects[] = [
        'uuid' => $p->uuid, 'name' => $p->name, 'description' => (string) col($p, 'description'),
        'environments' => array_values(array_map(fn($e) => ['id' => $e->id, 'name' => $e->name],
            array_filter($envs, fn($e) => $e->project_id == $p->id))),
    ];
}
$project_of = [];
foreach (rows('projects') as $p) $project_of[$p->id] = $p->uuid;
function where_env($r, $envs, $project_of) {
    $e = $envs[col($r, 'environment_id')] ?? null;
    return ['environment' => $e ? $e->name : 'production', 'project' => $e ? ($project_of[$e->project_id] ?? null) : null];
}

function vars($type, $id) {
    return array_map(fn($v) => [
        'key' => $v->key, 'value' => (string) dec($v->value), 'preview' => (bool) col($v, 'is_preview'),
        'build' => (bool) (col($v, 'is_buildtime') ?? col($v, 'is_build_time') ?? false),
        'runtime' => (bool) (col($v, 'is_runtime') ?? true), 'literal' => (bool) col($v, 'is_literal'),
    ], rows('environment_variables', ['resourceable_type' => $type, 'resourceable_id' => $id]));
}
function volumes($type, $id) {
    return array_map(fn($v) => ['name' => $v->name, 'mount_path' => $v->mount_path, 'host_path' => col($v, 'host_path') ?: null],
        rows('local_persistent_volumes', ['resource_type' => $type, 'resource_id' => $id]));
}

$keys = [];
foreach (rows('private_keys') as $k) $keys[$k->id] = $k;
$sources = [];
foreach (rows('github_apps') as $g) {
    $k = $keys[col($g, 'private_key_id')] ?? null;
    $sources["github:$g->id"] = [
        'key' => "github:$g->id", 'kind' => $g->is_public ? 'public' : 'github_app', 'name' => $g->name,
        'html_url' => $g->html_url, 'api_url' => $g->api_url, 'organization' => col($g, 'organization'),
        'app_id' => col($g, 'app_id'), 'installation_id' => col($g, 'installation_id'), 'client_id' => col($g, 'client_id'),
        'client_secret' => dec(col($g, 'client_secret')), 'webhook_secret' => dec(col($g, 'webhook_secret')),
        'private_key' => $k ? dec($k->private_key) : null,
    ];
}
foreach (rows('gitlab_apps') as $g) {
    $k = $keys[col($g, 'private_key_id')] ?? null;
    $sources["gitlab:$g->id"] = [
        'key' => "gitlab:$g->id", 'kind' => $k ? 'deploy_key' : 'public', 'name' => $g->name,
        'html_url' => $g->html_url, 'private_key' => $k ? dec($k->private_key) : null,
    ];
}

$apps = [];
foreach (rows('applications') as $a) {
    $set = DB::table('application_settings')->where('application_id', $a->id)->first();
    $src = null;
    if (col($a, 'private_key_id') && isset($keys[$a->private_key_id])) {
        $src = ['key' => "key:$a->private_key_id", 'kind' => 'deploy_key', 'name' => $keys[$a->private_key_id]->name,
            'private_key' => dec($keys[$a->private_key_id]->private_key)];
        $sources[$src['key']] = $src;
    } elseif (col($a, 'source_type') && col($a, 'source_id') !== null) {
        $t = str_contains($a->source_type, 'Gitlab') ? 'gitlab' : 'github';
        $src = $sources["$t:$a->source_id"] ?? null;
    }
    $apps[] = [
        'uuid' => $a->uuid, 'name' => $a->name, 'server' => server_of($a, $dest, $servers),
        'where' => where_env($a, $envs, $project_of), 'status' => (string) col($a, 'status'),
        'build_pack' => $a->build_pack, 'repository' => col($a, 'git_repository'), 'branch' => col($a, 'git_branch'),
        'commit' => col($a, 'git_commit_sha'), 'source' => $src ? $src['key'] : null,
        'base_directory' => col($a, 'base_directory'), 'publish_directory' => col($a, 'publish_directory'),
        'install_command' => col($a, 'install_command'), 'build_command' => col($a, 'build_command'),
        'start_command' => col($a, 'start_command'), 'dockerfile_location' => col($a, 'dockerfile_location'),
        'dockerfile' => col($a, 'dockerfile'), 'compose_location' => col($a, 'docker_compose_location'),
        'compose_raw' => col($a, 'docker_compose_raw'), 'compose_domains' => col($a, 'docker_compose_domains'),
        'image' => col($a, 'docker_registry_image_name'), 'image_tag' => col($a, 'docker_registry_image_tag'),
        'ports' => col($a, 'ports_exposes'), 'fqdn' => col($a, 'fqdn'),
        'health_path' => col($a, 'health_check_enabled') ? col($a, 'health_check_path') : null,
        'memory' => col($a, 'limits_memory'), 'cpus' => col($a, 'limits_cpus'), 'watch_paths' => col($a, 'watch_paths'),
        'auto_deploy' => $set ? (bool) $set->is_auto_deploy_enabled : true,
        'previews' => $set ? (bool) $set->is_preview_deployments_enabled : false,
        'basic_auth' => col($a, 'is_http_basic_auth_enabled')
            ? ['user' => (string) $a->http_basic_auth_username, 'password' => (string) dec($a->http_basic_auth_password)] : null,
        'envs' => vars('App\Models\Application', $a->id),
        'volumes' => volumes('App\Models\Application', $a->id),
        'tasks' => array_map(fn($t) => ['name' => $t->name, 'command' => $t->command, 'frequency' => $t->frequency,
            'enabled' => (bool) $t->enabled, 'container' => col($t, 'container')], rows('scheduled_tasks', ['application_id' => $a->id])),
    ];
}

$kinds = [
    'standalone_postgresqls' => ['postgresql', 'StandalonePostgresql', 'postgres_user', 'postgres_password', 'postgres_db'],
    'standalone_mysqls' => ['mysql', 'StandaloneMysql', 'mysql_user', 'mysql_password', 'mysql_database'],
    'standalone_mariadbs' => ['mariadb', 'StandaloneMariadb', 'mariadb_user', 'mariadb_password', 'mariadb_database'],
    'standalone_mongodbs' => ['mongodb', 'StandaloneMongodb', 'mongo_initdb_root_username', 'mongo_initdb_root_password', 'mongo_initdb_database'],
    'standalone_redis' => ['redis', 'StandaloneRedis', null, 'redis_password', null],
    'standalone_keydbs' => ['keydb', 'StandaloneKeydb', null, 'keydb_password', null],
    'standalone_dragonflies' => ['dragonfly', 'StandaloneDragonfly', null, 'dragonfly_password', null],
    'standalone_clickhouses' => ['clickhouse', 'StandaloneClickhouse', 'clickhouse_admin_user', 'clickhouse_admin_password', 'clickhouse_db'],
];
$dbs = [];
foreach ($kinds as $table => [$kind, $model, $u, $p, $d]) {
    foreach (rows($table) as $r) {
        $env = vars("App\\Models\\$model", $r->id);
        $password = dec(col($r, $p));
        // Redis keeps its password in a variable of the database.
        foreach ($env as $v) if (in_array($v['key'], ['REDIS_PASSWORD', 'KEYDB_PASSWORD', 'DRAGONFLY_PASSWORD'])) $password = $v['value'];
        $dbs[] = [
            'uuid' => $r->uuid, 'name' => $r->name, 'kind' => $kind, 'image' => $r->image,
            'user' => $u ? dec(col($r, $u)) : null, 'password' => (string) $password, 'database' => $d ? dec(col($r, $d)) : null,
            'public_port' => col($r, 'is_public') ? col($r, 'public_port') : null, 'status' => (string) col($r, 'status'),
            'server' => server_of($r, $dest, $servers), 'where' => where_env($r, $envs, $project_of),
            'volumes' => volumes("App\\Models\\$model", $r->id),
        ];
    }
}

$services = [];
foreach (rows('services') as $s) {
    $services[] = [
        'uuid' => $s->uuid, 'name' => $s->name, 'where' => where_env($s, $envs, $project_of),
        'server' => $servers[col($s, 'server_id') ?? 0] ?? ['name' => '?', 'ip' => '', 'local' => false],
        'compose_raw' => col($s, 'docker_compose_raw'),
        'routes' => array_values(array_filter(array_map(fn($a) => ['service' => $a->name, 'fqdn' => col($a, 'fqdn')],
            rows('service_applications', ['service_id' => $s->id])), fn($r) => !empty($r['fqdn']))),
        'envs' => vars('App\Models\Service', $s->id),
    ];
}

$shared = array_map(fn($v) => ['key' => $v->key, 'value' => (string) dec($v->value), 'type' => $v->type,
    'project' => $project_of[col($v, 'project_id')] ?? null, 'environment' => ($envs[col($v, 'environment_id')] ?? null)?->name],
    rows('shared_environment_variables'));

echo json_encode([
    'version' => config('constants.coolify.version'),
    'projects' => $projects, 'apps' => $apps, 'databases' => $dbs, 'services' => $services,
    'sources' => array_values($sources), 'shared' => $shared, 'servers' => array_values($servers),
], JSON_UNESCAPED_SLASHES), "\n";
