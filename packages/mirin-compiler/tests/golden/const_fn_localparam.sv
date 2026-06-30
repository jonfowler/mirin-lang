module widen #(parameter int N) (
    input  logic [7:0] x,
    output logic [7:0] result
);
    function automatic int sum_to(input int n);
        int acc;
        acc = 0;
        for (int i = 0; i < n; i++) begin
            acc = (acc + i);
        end
        return acc;
    endfunction
    localparam int w = sum_to(N);
    logic [w-1:0] tmp;
    logic [w-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x);
    assign tmp = __block_0;
    logic [7:0] __block_1;
    assign __block_1 = type(__block_1)'(tmp);
    assign result = __block_1;
endmodule
