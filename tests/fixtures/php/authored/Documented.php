<?php

declare(strict_types=1);

namespace App\Documented;

/**
 * A documented service.
 */
final class Documented
{
    /**
     * Runs the documented work.
     */
    public function run(): void
    {
    }

    public function plain(): void
    {
    }

    /**
     * This docblock is separated by a blank line.
     */

    public function separated(): void
    {
    }
}
