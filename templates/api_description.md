### Overview

Welcome to the Klappstuhl.me API. You can use this API to access the contents of the site. The API is based off of a simple REST API with a few endpoints.

### Authentication

Klappstuhl.me uses API keys to allow access to the API. Authentication is done using the `Authorization` header. Note that in order to use this API, an account is required. Please [register](/login) if you have not done so already.

If you have not generated an API key yet, you can do so on your [account page](/account).

### Rate Limits

Rate limits are enforced at an IP level to prevent abuse and spam on the service. When a rate limit is hit, an HTTP 429 status code is returned with some header information telling you how to proceed.

#### Header Format

The following headers are returned when using a rate limited endpoint:

```
x-ratelimit-limit: 25
x-ratelimit-remaining: 14
x-ratelimit-reset: 1713373688
x-ratelimit-reset-after: 0.98
```
- **x-ratelimit-limit**: The number of requests that can be made.
- **x-ratelimit-remaining**: How many requests are left before hitting a 429.
- **x-ratelimit-reset**: The UNIX timestamp (seconds since midnight UTC on January 1st 1970) at which the rate limit resets. This can have a fractional component for milliseconds.
- **x-ratelimit-reset-after**: The total time in seconds to wait for the rate limit to restart. This can have a fractional component for milliseconds.