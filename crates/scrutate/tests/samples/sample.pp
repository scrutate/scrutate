# Puppet sample
# Installs nginx.
class nginx (
  String $version = 'latest',
) {
  package { 'nginx': ensure => $version }
  notice($version)
}
define nginx::vhost (String $server) {
  file { "/etc/nginx/${server}": ensure => file }
}
function nginx::port(String $server) >> Integer {
  80
}
node 'web.example.com' {
  include nginx
}
