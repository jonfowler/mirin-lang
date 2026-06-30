module chain #(parameter int N) (
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
    localparam int a = sum_to(N);
    localparam int b = sum_to(a);
    logic [a-1:0] t1;
    logic [b-1:0] t2;
    logic [a-1:0] __block_0;
    assign __block_0 = (8 != 0) ? type(__block_0)'(x) : '0;
    assign t1 = __block_0;
    logic [b-1:0] __block_1;
    assign __block_1 = (a != 0) ? type(__block_1)'(t1) : '0;
    assign t2 = __block_1;
    logic [7:0] __block_2;
    assign __block_2 = (b != 0) ? type(__block_2)'(t2) : '0;
    assign result = __block_2;
endmodule
