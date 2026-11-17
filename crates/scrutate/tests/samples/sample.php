<?php
namespace App;

use Psr\Log\LoggerInterface;

/** Greets users. */
function greet(string $name): string { return "Hi $name"; }

class User {
    public function name(): string { return $this->first; }
    public static function make(): self { return new self(); }
}
interface Named { public function name(): string; }
trait Timestamps { public function touch() {} }
enum Status { case Active; }
$u = User::make();
echo greet($u->name());
